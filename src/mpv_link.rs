//! Every conversation with mpv (except drawing) happens on this background thread.
//!
//! mpv's embedding docs require that the thread which draws the video never waits
//! for mpv's core, or the two can deadlock. Reading a property (time, volume, ...)
//! is such a wait. Old mpv (0.36 / early 2024) never made its core wait for the
//! drawing thread, so it didn't matter; newer mpv does, e.g. right after the
//! window size changes. Then our UI thread waited for the core, the core waited
//! for the video thread, and the video thread waited for us to draw, until a
//! 200 ms timeout broke it ("mpv_render_context_render() not being called or
//! stuck" in mpv's log). That was the resize hitching.
//!
//! So the UI never calls mpv directly any more: it queues requests here and reads
//! the latest state from a snapshot that this thread refreshes ~10 times a second.

use libmpv2::Mpv;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The player state as last seen by the worker
#[derive(Clone, Default)]
pub struct Snapshot {
    /// False until the first refresh has happened
    pub valid: bool,
    pub time_pos: f64,
    pub duration: f64,
    pub volume: f64,
    pub paused: bool,
    pub muted: bool,
    pub eof: bool,
    /// Decoder / video details, only filled while the info overlay is open
    pub info: Vec<String>,
}

enum Job {
    Set(String, String),
    Command(String, Vec<String>),
    /// true: switch on the live-resize settings (saving the current ones); false: restore
    ResizeMode(bool),
}

pub struct MpvLink {
    tx: mpsc::Sender<Job>,
    state: Arc<Mutex<Snapshot>>,
    want_info: Arc<AtomicBool>,
}

/// mpv's "yes" / "no"
pub fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

impl MpvLink {
    pub fn start(mpv: Arc<Mutex<Mpv>>) -> Self {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let want_info = Arc::new(AtomicBool::new(false));
        let (st, wi) = (state.clone(), want_info.clone());
        std::thread::Builder::new()
            .name("mpv-link".into())
            .spawn(move || run(mpv, rx, st, wi))
            .expect("could not start the mpv worker thread");
        Self { tx, state, want_info }
    }

    /// Set a property (any value mpv accepts as text: "yes", "80", "12.5", ...)
    pub fn set(&self, name: &str, value: impl ToString) {
        let _ = self.tx.send(Job::Set(name.to_owned(), value.to_string()));
    }

    pub fn command(&self, name: &str, args: &[&str]) {
        let _ = self.tx.send(Job::Command(name.to_owned(), args.iter().map(|s| s.to_string()).collect()));
    }

    pub fn resize_mode(&self, on: bool) {
        let _ = self.tx.send(Job::ResizeMode(on));
    }

    pub fn want_info(&self, on: bool) {
        self.want_info.store(on, Ordering::Relaxed);
    }

    /// The most recent state. Never waits for mpv.
    pub fn snapshot(&self) -> Snapshot {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// Settings switched off during a live window resize: both put the window size
/// (height / shrink ratio) into mpv's shaders, forcing a recompile per new size.
const RESIZE_SETTINGS: [(&str, &str); 2] = [("dither-depth", "no"), ("correct-downscaling", "no")];

fn run(mpv: Arc<Mutex<Mpv>>, rx: mpsc::Receiver<Job>, state: Arc<Mutex<Snapshot>>, want_info: Arc<AtomicBool>) {
    let mut resize_saved: Option<Vec<(String, String)>> = None;
    loop {
        // Wait for work, or refresh the snapshot every 100 ms
        let mut jobs = Vec::new();
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(job) => jobs.push(job),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(job) = rx.try_recv() {
            jobs.push(job);
        }
        // For repeated settings of the same property (e.g. dragging the seek bar)
        // only the last one matters
        let mut last_set: HashMap<String, usize> = HashMap::new();
        for (i, job) in jobs.iter().enumerate() {
            if let Job::Set(name, _) = job {
                last_set.insert(name.clone(), i);
            }
        }

        let Ok(m) = mpv.lock() else { return };
        for (i, job) in jobs.into_iter().enumerate() {
            match job {
                Job::Set(name, value) => {
                    if last_set.get(&name) == Some(&i) {
                        let _ = m.set_property(&name, value.as_str());
                    }
                }
                Job::Command(name, args) => {
                    let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    let _ = m.command(&name, &args);
                }
                Job::ResizeMode(true) => {
                    if resize_saved.is_none() {
                        let mut saved = Vec::new();
                        for (k, v) in RESIZE_SETTINGS {
                            if let Ok(old) = m.get_property::<String>(k) {
                                if m.set_property(k, v).is_ok() {
                                    saved.push((k.to_owned(), old));
                                }
                            }
                        }
                        resize_saved = Some(saved);
                    }
                }
                Job::ResizeMode(false) => {
                    if let Some(saved) = resize_saved.take() {
                        for (k, v) in &saved {
                            let _ = m.set_property(k, v.as_str());
                        }
                    }
                }
            }
        }

        let mut s = Snapshot {
            valid: true,
            time_pos: m.get_property::<f64>("time-pos").unwrap_or(0.0),
            duration: m.get_property::<f64>("duration").unwrap_or(0.0),
            volume: m.get_property::<f64>("volume").unwrap_or(0.0),
            paused: m.get_property::<bool>("pause").unwrap_or(true),
            muted: m.get_property::<bool>("mute").unwrap_or(false),
            eof: m.get_property::<bool>("eof-reached").unwrap_or(false),
            info: Vec::new(),
        };
        if want_info.load(Ordering::Relaxed) {
            let hw = m.get_property::<String>("hwdec-current").unwrap_or_default();
            s.info.push(match hw.as_str() {
                "" => "Decoder: starting…".to_owned(),
                "no" => "Decoder: software (CPU)".to_owned(),
                other => format!("Decoder: hardware ({other})"),
            });
            let fmt = m.get_property::<String>("video-format").unwrap_or_default();
            let w = m.get_property::<i64>("width").unwrap_or(0);
            let h = m.get_property::<i64>("height").unwrap_or(0);
            let fps = m.get_property::<f64>("estimated-vf-fps").unwrap_or(0.0);
            s.info.push(format!("Video: {fmt} {w}x{h} @ {fps:.1} fps"));
            let dropped = m.get_property::<i64>("frame-drop-count").unwrap_or(0);
            let dec_dropped = m.get_property::<i64>("decoder-frame-drop-count").unwrap_or(0);
            s.info.push(format!("Dropped frames: {dropped} (decoder: {dec_dropped})"));
            let ahead = m.get_property::<f64>("demuxer-cache-duration").unwrap_or(0.0);
            s.info.push(format!("Buffered ahead: {ahead:.1} s"));
            let version = m.get_property::<String>("mpv-version").unwrap_or_default();
            s.info.push(format!("Engine: {version}"));
        }
        drop(m);

        if let Ok(mut st) = state.lock() {
            *st = s;
        }
    }
}
