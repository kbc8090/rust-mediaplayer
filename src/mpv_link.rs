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
use std::time::{Duration, Instant};

/// The player state as last seen by the worker
#[derive(Clone, Default)]
pub struct Snapshot {
    /// False until the first refresh has happened
    pub valid: bool,
    /// When `time_pos` was read (the UI moves the seek bar on smoothly from there)
    pub taken: Option<Instant>,
    pub time_pos: f64,
    pub duration: f64,
    pub volume: f64,
    pub paused: bool,
    pub muted: bool,
    pub eof: bool,
    /// Displayed video size (0 for audio-only / nothing loaded)
    pub dwidth: i64,
    pub dheight: i64,
    /// The video's frame rate (0 if unknown / no video)
    pub fps: f64,
    /// The file these details belong to (as passed to loadfile)
    pub path: String,
    /// No file loaded (also after a file / link failed to open)
    pub idle: bool,
    /// The title mpv knows the file by (a link's video title, once yt-dlp found it)
    pub media_title: String,
    /// Chapters (start time in seconds, title), empty if the file has none
    pub chapters: Arc<Vec<(f64, String)>>,
    /// Video / audio details, only filled while the info overlay is open
    pub info: Vec<InfoRow>,
    /// The last link's details from yt-dlp (or why it failed), once known
    pub link_info: Option<Arc<crate::online::LinkInfo>>,
}

/// A line of the info overlay
#[derive(Clone)]
pub enum InfoRow {
    /// A heading ("Video", "Audio", ...)
    Section(&'static str),
    /// Label and value
    Item(&'static str, String),
}

enum Job {
    Set(String, String),
    Command(String, Vec<String>),
    /// true: switch on the live-resize settings (saving the current ones); false: restore
    ResizeMode(bool),
    /// Nothing to do, just refresh the snapshot now (instead of at the next tick)
    Wake,
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
        // Just switched on: refresh right away, so the overlay has its details
        // within a moment instead of at the next 100 ms tick
        if !self.want_info.swap(on, Ordering::Relaxed) && on {
            let _ = self.tx.send(Job::Wake);
        }
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
    // Chapters are re-read only when the file or the chapter count changes
    let mut chapter_key: (String, i64) = (String::new(), -1);
    let mut chapters: Arc<Vec<(f64, String)>> = Arc::new(Vec::new());
    // Just after a file is opened, the snapshot is refreshed more often, so its
    // length, chapters and picture size reach the UI without waiting a full tick
    let mut quick_until: Option<Instant> = None;
    // Software fallback check (see below): which file, since when, and whether done
    let mut fallback_check: (String, Instant, bool) = (String::new(), Instant::now(), false);
    // yt-dlp's answer about the last link: which link was read, the link being
    // opened (kept while mpv is idle after a failure), and what was found
    let (mut link_read, mut last_link) = (String::new(), String::new());
    let mut link_info: Option<Arc<crate::online::LinkInfo>> = None;
    loop {
        // Wait for work, or refresh the snapshot every 100 ms (25 ms just after a
        // file was opened)
        let quick = quick_until.is_some_and(|t| Instant::now() < t);
        let tick = Duration::from_millis(if quick { 25 } else { 100 });
        let mut jobs = Vec::new();
        match rx.recv_timeout(tick) {
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
                    if name == "hwdec" {
                        // New decoding method: check the fallback again
                        fallback_check = (String::new(), Instant::now(), false);
                    }
                }
                Job::Command(name, args) => {
                    if name == "loadfile" {
                        quick_until = Some(Instant::now() + Duration::from_millis(1500));
                        // A previous file may have needed software threads (see below);
                        // a hardware decoder gets its single thread back
                        if m.get_property::<String>("hwdec").is_ok_and(|h| h != "no") {
                            let _ = m.set_property("vd-lavc-threads", 1_i64);
                        }
                    }
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
                Job::Wake => {}
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
            taken: Some(Instant::now()),
            time_pos: m.get_property::<f64>("time-pos").unwrap_or(0.0),
            duration: m.get_property::<f64>("duration").unwrap_or(0.0),
            volume: m.get_property::<f64>("volume").unwrap_or(0.0),
            paused: m.get_property::<bool>("pause").unwrap_or(true),
            muted: m.get_property::<bool>("mute").unwrap_or(false),
            eof: m.get_property::<bool>("eof-reached").unwrap_or(false),
            dwidth: m.get_property::<i64>("dwidth").unwrap_or(0),
            dheight: m.get_property::<i64>("dheight").unwrap_or(0),
            fps: m
                .get_property::<f64>("container-fps")
                .ok()
                .filter(|f| *f > 0.0)
                .or_else(|| m.get_property::<f64>("estimated-vf-fps").ok())
                .unwrap_or(0.0),
            chapters: chapters.clone(),
            info: Vec::new(),
            path: String::new(),
            idle: m.get_property::<bool>("idle-active").unwrap_or(false),
            media_title: m.get_property::<String>("media-title").unwrap_or_default(),
            link_info: None,
        };
        let key = (
            m.get_property::<String>("path").unwrap_or_default(),
            m.get_property::<i64>("chapter-list/count").unwrap_or(0),
        );
        let path = key.0.clone();
        s.path = path.clone();
        if key != chapter_key {
            let mut list = Vec::new();
            for i in 0..key.1 {
                let time = m.get_property::<f64>(&format!("chapter-list/{i}/time")).unwrap_or(0.0);
                let title = m
                    .get_property::<String>(&format!("chapter-list/{i}/title"))
                    .ok()
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| format!("Chapter {}", i + 1));
                list.push((time, title));
            }
            chapters = Arc::new(list);
            s.chapters = chapters.clone();
            chapter_key = key;
        }
        // Hardware decoding was asked for, but this file's format can't be decoded
        // by the GPU (AV1 on older cards, say): mpv quietly decodes it in software,
        // still held to the single thread meant for the hardware decoder, and 1440p
        // or 4K at 60 fps can't keep up on one core. Give it all threads back and
        // reload the video track. (Checked for the first seconds of each file: mpv
        // can also give up on the GPU a moment into decoding.)
        if fallback_check.0 != path {
            fallback_check = (path, Instant::now(), false);
        }
        if !fallback_check.2 && fallback_check.1.elapsed() < Duration::from_secs(4) {
            let wanted = m.get_property::<String>("hwdec").unwrap_or_default();
            let current = m.get_property::<String>("hwdec-current");
            let image = m.get_property::<bool>("current-tracks/video/image").unwrap_or(false);
            if wanted != "no"
                && current.as_deref() == Ok("no")
                && !image
                && m.get_property::<i64>("vd-lavc-threads").unwrap_or(0) == 1
            {
                let _ = m.set_property("vd-lavc-threads", 0_i64);
                let _ = m.command("video-reload", &[]);
                fallback_check.2 = true;
            }
        }
        // A link: once it plays (or mpv gives up on it), read what yt-dlp said
        // about it, once. (The app clears mpv's copy before opening each link.)
        if crate::online::is_url(&s.path) {
            last_link.clone_from(&s.path);
        } else if !s.path.is_empty() {
            last_link.clear();
            link_info = None;
        }
        let ready = if crate::online::is_url(&s.path) && (s.duration > 0.0 || s.dwidth > 0) {
            Some(s.path.clone())
        } else if s.idle && !last_link.is_empty() {
            Some(last_link.clone())
        } else {
            None
        };
        if let Some(url) = ready.filter(|u| *u != link_read) {
            let text = m.get_property::<String>(crate::online::YTDL_RESULT).unwrap_or_default();
            link_info = crate::online::parse_link_result(&text).map(|mut info| {
                info.url.clone_from(&url);
                Arc::new(info)
            });
            link_read = url;
        }
        s.link_info = link_info.clone();
        if want_info.load(Ordering::Relaxed) {
            s.info = read_info(&m);
            if let Some(li) = link_info.as_ref().filter(|li| li.url == s.path && !li.rows.is_empty()) {
                s.info.push(InfoRow::Section("Online"));
                for (label, value) in &li.rows {
                    s.info.push(InfoRow::Item(label, value.clone()));
                }
            }
        }
        drop(m);

        if let Ok(mut st) = state.lock() {
            *st = s;
        }
    }
}

/// Everything the info overlay shows about the file and playback
fn read_info(m: &Mpv) -> Vec<InfoRow> {
    use InfoRow::{Item, Section};
    let text = |k: &str| m.get_property::<String>(k).ok().filter(|v| !v.is_empty());
    let float = |k: &str| m.get_property::<f64>(k).ok();
    let int = |k: &str| m.get_property::<i64>(k).ok();
    let mut info = Vec::new();

    // ── Video
    info.push(Section("Video"));
    let mut fmt = Vec::new();
    if let Some(c) = text("video-format") {
        fmt.push(codec_name(&c));
    }
    if let (Some(w), Some(h)) = (int("width"), int("height")) {
        if w > 0 && h > 0 {
            fmt.push(format!("{w}\u{d7}{h}"));
        }
    }
    if let Some(f) = float("container-fps").filter(|f| *f > 0.0).or_else(|| float("estimated-vf-fps")) {
        fmt.push(format!("{} fps", short_num(f)));
    }
    if let Some(pf) = text("video-params/pixelformat") {
        fmt.push(format!("{}-bit", bit_depth(&pf)));
    }
    match text("video-params/gamma").as_deref() {
        Some("pq") => fmt.push("HDR10".into()),
        Some("hlg") => fmt.push("HLG".into()),
        _ => {}
    }
    info.push(Item("Format", if fmt.is_empty() { "\u{2013}".into() } else { fmt.join("  \u{b7}  ") }));
    info.push(Item("Bitrate", float("video-bitrate").map_or("measuring\u{2026}".into(), bitrate)));
    let threads = match int("vd-lavc-threads") {
        Some(1) => "1 thread".to_owned(),
        Some(n) if n > 1 => format!("{n} threads"),
        _ => "all threads".to_owned(),
    };
    info.push(Item(
        "Decoder",
        match text("hwdec-current").as_deref() {
            None => "starting\u{2026}".to_owned(),
            Some("no") => format!("Software (CPU)  \u{b7}  {threads}"),
            Some(other) => format!("Hardware ({other})"),
        },
    ));
    let dropped = int("frame-drop-count").unwrap_or(0);
    let dec_dropped = int("decoder-frame-drop-count").unwrap_or(0);
    let late = int("vo-delayed-frame-count").unwrap_or(0);
    let mistimed = int("mistimed-frame-count").unwrap_or(0);
    info.push(Item(
        "Dropped",
        format!("{dropped}  \u{b7}  decoder {dec_dropped}  \u{b7}  late {late}  \u{b7}  mistimed {mistimed}"),
    ));

    // ── Audio
    info.push(Section("Audio"));
    let has_audio = match text("audio-codec-name") {
        Some(codec) => {
            let mut a = vec![codec_name(&codec)];
            if let Some(sr) = int("audio-params/samplerate") {
                a.push(format!("{} kHz", short_num(sr as f64 / 1000.0)));
            }
            if let Some(ch) = text("audio-params/hr-channels") {
                a.push(channels_name(&ch));
            }
            info.push(Item("Format", a.join("  \u{b7}  ")));
            info.push(Item("Bitrate", float("audio-bitrate").map_or("measuring\u{2026}".into(), bitrate)));
            let mut o = Vec::new();
            if let Some(ao) = text("current-ao") {
                o.push(ao.to_uppercase());
            }
            if let Some(sr) = int("audio-out-params/samplerate") {
                o.push(format!("{} kHz", short_num(sr as f64 / 1000.0)));
            }
            if let Some(f) = text("audio-out-params/format") {
                o.push(sample_format_name(&f));
            }
            info.push(Item("Output", if o.is_empty() { "\u{2013}".into() } else { o.join("  \u{b7}  ") }));
            true
        }
        None => {
            info.push(Item("Format", "No audio track".into()));
            false
        }
    };

    // ── Playback
    info.push(Section("Playback"));
    let display = float("display-fps").unwrap_or(0.0);
    let sync = match text("video-sync").as_deref() {
        Some("display-resample") => {
            if m.get_property::<bool>("interpolation").unwrap_or(false) {
                "smooth motion"
            } else {
                "timed to display"
            }
        }
        _ => "timed to audio",
    };
    info.push(Item(
        "Display",
        if display > 0.0 { format!("{display:.0} Hz  \u{b7}  {sync}") } else { sync.to_owned() },
    ));
    if has_audio {
        let avsync = float("avsync").unwrap_or(0.0) * 1000.0;
        info.push(Item("A/V offset", format!("{avsync:+.0} ms")));
    }
    let scale = text("scale").unwrap_or_default();
    let dscale = text("dscale").unwrap_or_default();
    info.push(Item("Scaling", format!("up {scale}  \u{b7}  down {dscale}")));
    let ahead = float("demuxer-cache-duration").unwrap_or(0.0);
    info.push(Item("Buffered", format!("{ahead:.1} s ahead")));
    if let Some(v) = text("mpv-version") {
        info.push(Item("Engine", v));
    }
    info
}

/// "VP9", "H.264", "Opus", ... from FFmpeg's codec names
fn codec_name(c: &str) -> String {
    let c = c.to_ascii_lowercase();
    match c.as_str() {
        "h264" => "H.264",
        "hevc" | "h265" => "HEVC",
        "av1" => "AV1",
        "vp9" => "VP9",
        "vp8" => "VP8",
        "mpeg2video" => "MPEG-2",
        "mpeg4" => "MPEG-4",
        "prores" => "ProRes",
        "aac" => "AAC",
        "opus" => "Opus",
        "mp3" | "mp3float" => "MP3",
        "flac" => "FLAC",
        "vorbis" => "Vorbis",
        "ac3" => "Dolby Digital",
        "eac3" => "Dolby Digital Plus",
        "truehd" => "Dolby TrueHD",
        "dts" => "DTS",
        "alac" => "ALAC",
        "wmav2" | "wmapro" => "WMA",
        _ if c.starts_with("pcm") => "PCM",
        _ => return c.to_uppercase(),
    }
    .to_owned()
}

/// "Stereo", "5.1", ... from mpv's channel layout names
fn channels_name(ch: &str) -> String {
    match ch {
        "mono" => "Mono".to_owned(),
        "stereo" => "Stereo".to_owned(),
        other => other.to_owned(),
    }
}

/// "32-bit float", "16-bit", ... from mpv's sample format names
fn sample_format_name(f: &str) -> String {
    let base = f.trim_end_matches('p');
    match base {
        "float" => "32-bit float".to_owned(),
        "double" => "64-bit float".to_owned(),
        "s16" => "16-bit".to_owned(),
        "s24" => "24-bit".to_owned(),
        "s32" => "32-bit".to_owned(),
        "s64" => "64-bit".to_owned(),
        "u8" => "8-bit".to_owned(),
        _ if f.starts_with("spdif") => "passthrough".to_owned(),
        _ => f.to_owned(),
    }
}

/// Bits per colour from a pixel format name ("yuv420p10" is 10, "p010" is 10,
/// "nv12" / "yuv420p" are 8)
fn bit_depth(pf: &str) -> u32 {
    if pf.starts_with("p010") {
        return 10;
    }
    if pf.starts_with("p016") {
        return 16;
    }
    let digits: String = pf.chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    // (only 9..16 are bit depths: "rgb24" or "bgr0" end in digits that aren't)
    digits.parse::<u32>().ok().filter(|n| (9..=16).contains(n)).unwrap_or(8)
}

/// "18.4 Mb/s" or "160 kb/s"
fn bitrate(bits_per_sec: f64) -> String {
    if bits_per_sec >= 1_000_000.0 {
        format!("{:.1} Mb/s", bits_per_sec / 1_000_000.0)
    } else {
        format!("{:.0} kb/s", bits_per_sec / 1000.0)
    }
}

/// 60 → "60", 23.976 → "23.976", 44.1 → "44.1"
fn short_num(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}
