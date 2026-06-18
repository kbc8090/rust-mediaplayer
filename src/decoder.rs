// mediaplayer/src/decoder.rs
// FFmpeg decoding + rodio audio output, runs on a dedicated thread

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::Mutex;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use anyhow::{Result, Context};

use ffmpeg_next as ffmpeg;
use ffmpeg::format::{input, Pixel};
use ffmpeg::media::Type;
use ffmpeg::software::scaling::{context::Context as SwsContext, flag::Flags};
use ffmpeg::util::frame::video::Video;

use rodio::{OutputStream, Sink, Source};

use crate::player::{PlaybackState, PlayerCommand, PlayerEvent, PlayState};

pub fn player_thread(
    cmd_rx: Receiver<PlayerCommand>,
    evt_tx: Sender<PlayerEvent>,
    state: Arc<Mutex<PlaybackState>>,
) {
    let mut session: Option<PlaySession> = None;

    loop {
        // Drain all pending commands
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    handle_command(
                        cmd,
                        &mut session,
                        &evt_tx,
                        &state,
                        &cmd_rx,
                    );
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // Advance playback if active
        if let Some(ref mut sess) = session {
            match advance_frame(sess, &evt_tx, &state) {
                Ok(true) => {} // frame produced
                Ok(false) => {
                    // End of file
                    let repeat = state.lock().repeat;
                    if repeat {
                        if let Err(e) = sess.seek(0.0) {
                            let _ = evt_tx.send(PlayerEvent::Error(e.to_string()));
                        }
                    } else {
                        state.lock().state = PlayState::Ended;
                        let _ = evt_tx.send(PlayerEvent::Ended);
                        session = None;
                    }
                }
                Err(e) => {
                    let _ = evt_tx.send(PlayerEvent::Error(e.to_string()));
                    session = None;
                }
            }
        } else {
            // Nothing to do, sleep briefly to avoid spinning
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn handle_command(
    cmd: PlayerCommand,
    session: &mut Option<PlaySession>,
    evt_tx: &Sender<PlayerEvent>,
    state: &Arc<Mutex<PlaybackState>>,
    _cmd_rx: &Receiver<PlayerCommand>,
) {
    match cmd {
        PlayerCommand::Open(path) => {
            *session = None;
            match PlaySession::open(&path, state) {
                Ok(mut sess) => {
                    let duration = sess.duration;
                    // Grab thumbnail (first frame)
                    let _ = sess.decode_thumbnail(state);
                    {
                        let mut s = state.lock();
                        s.duration = duration;
                        s.position = 0.0;
                        s.current_file = Some(path.clone());
                        s.state = PlayState::Playing;
                    }
                    let _ = evt_tx.send(PlayerEvent::Opened { duration, path });
                    let _ = evt_tx.send(PlayerEvent::Playing);
                    *session = Some(sess);
                }
                Err(e) => {
                    let _ = evt_tx.send(PlayerEvent::Error(format!("Cannot open file: {e}")));
                }
            }
        }

        PlayerCommand::Play => {
            if let Some(ref mut sess) = session {
                sess.paused = false;
                state.lock().state = PlayState::Playing;
                let _ = evt_tx.send(PlayerEvent::Playing);
            }
        }

        PlayerCommand::Pause => {
            if let Some(ref mut sess) = session {
                sess.paused = true;
                state.lock().state = PlayState::Paused;
                let _ = evt_tx.send(PlayerEvent::Paused);
            }
        }

        PlayerCommand::SeekTo(secs) => {
            if let Some(ref mut sess) = session {
                let _ = sess.seek(secs);
                state.lock().position = secs;
            }
        }

        PlayerCommand::SkipForward(secs) => {
            if let Some(ref mut sess) = session {
                let new_pos = (state.lock().position + secs).min(sess.duration);
                let _ = sess.seek(new_pos);
                state.lock().position = new_pos;
            }
        }

        PlayerCommand::SkipBack(secs) => {
            if let Some(ref mut sess) = session {
                let new_pos = (state.lock().position - secs).max(0.0);
                let _ = sess.seek(new_pos);
                state.lock().position = new_pos;
            }
        }

        PlayerCommand::SetVolume(vol) => {
            if let Some(ref mut sess) = session {
                sess.set_volume(vol);
            }
            state.lock().volume = vol;
        }

        PlayerCommand::Stop => {
            *session = None;
            state.lock().state = PlayState::Idle;
        }

        PlayerCommand::Next | PlayerCommand::Prev => {
            // Playlist logic handled in app.rs before sending Open
        }
    }
}

// ─── PlaySession ─────────────────────────────────────────────────────────────

struct PlaySession {
    ictx: ffmpeg::format::context::Input,
    video_stream_idx: usize,
    video_decoder: ffmpeg::decoder::Video,
    scaler: SwsContext,
    pub duration: f64,
    pub paused: bool,
    last_pts: f64,
    wall_start: Instant,
    pts_start: f64,

    // Audio
    _audio_stream: Option<OutputStream>,
    audio_sink: Option<Sink>,
    audio_stream_idx: Option<usize>,
}

impl PlaySession {
    fn open(path: &PathBuf, state: &Arc<Mutex<PlaybackState>>) -> Result<Self> {
        let ictx = input(path).context("ffmpeg: cannot open input")?;

        // ── Video stream ──
        let video_stream = ictx
            .streams()
            .best(Type::Video)
            .ok_or_else(|| anyhow::anyhow!("No video stream found"))?;
        let video_stream_idx = video_stream.index();

        let video_ctx = ffmpeg::codec::context::Context::from_parameters(
            video_stream.parameters(),
        )?;
        let video_decoder = video_ctx.decoder().video()?;

        let scaler = SwsContext::get(
            video_decoder.format(),
            video_decoder.width(),
            video_decoder.height(),
            Pixel::RGBA,
            video_decoder.width(),
            video_decoder.height(),
            Flags::BILINEAR,
        )?;

        let duration = ictx.duration() as f64 / ffmpeg::ffi::AV_TIME_BASE as f64;
        let volume = state.lock().volume;

        // ── Audio stream via rodio ──
        let audio_stream_idx = ictx
            .streams()
            .best(Type::Audio)
            .map(|s| s.index());

        let (_audio_stream, audio_sink) = match OutputStream::try_default() {
            Ok((stream, handle)) => {
                let sink = Sink::try_new(&handle).ok();
                if let Some(ref s) = sink {
                    s.set_volume(volume);
                }
                (Some(stream), sink)
            }
            Err(_) => (None, None),
        };

        Ok(Self {
            ictx,
            video_stream_idx,
            video_decoder,
            scaler,
            duration,
            paused: false,
            last_pts: 0.0,
            wall_start: Instant::now(),
            pts_start: 0.0,
            _audio_stream,
            audio_sink,
            audio_stream_idx,
        })
    }

    fn seek(&mut self, secs: f64) -> Result<()> {
        let ts = (secs * ffmpeg::ffi::AV_TIME_BASE as f64) as i64;
        unsafe {
            ffmpeg::ffi::av_seek_frame(
                self.ictx.as_mut_ptr(),
                -1,
                ts,
                ffmpeg::ffi::AVSEEK_FLAG_BACKWARD as i32,
            );
        }
        self.video_decoder.flush();
        self.last_pts = secs;
        self.wall_start = Instant::now();
        self.pts_start = secs;
        Ok(())
    }

    fn set_volume(&mut self, vol: f32) {
        if let Some(ref sink) = self.audio_sink {
            sink.set_volume(vol);
        }
    }

    /// Decode the very first video frame as thumbnail
    fn decode_thumbnail(&mut self, state: &Arc<Mutex<PlaybackState>>) -> Result<()> {
        for (stream, packet) in self.ictx.packets() {
            if stream.index() == self.video_stream_idx {
                self.video_decoder.send_packet(&packet)?;
                let mut decoded = Video::empty();
                if self.video_decoder.receive_frame(&mut decoded).is_ok() {
                    let mut rgb_frame = Video::empty();
                    self.scaler.run(&decoded, &mut rgb_frame)?;
                    let w = rgb_frame.width();
                    let h = rgb_frame.height();
                    let data = rgb_frame.data(0).to_vec();
                    state.lock().thumbnail_rgba = Some((data, w, h));
                    break;
                }
            }
        }
        // Seek back to start
        self.seek(0.0)?;
        Ok(())
    }
}

/// Returns Ok(true) if a frame was produced, Ok(false) on EOF
fn advance_frame(
    sess: &mut PlaySession,
    evt_tx: &Sender<PlayerEvent>,
    state: &Arc<Mutex<PlaybackState>>,
) -> Result<bool> {
    if sess.paused {
        std::thread::sleep(Duration::from_millis(8));
        return Ok(true);
    }

    // Simple AV sync: wall-clock pacing
    let wall_elapsed = sess.wall_start.elapsed().as_secs_f64();
    let target_pts = sess.pts_start + wall_elapsed;

    // Read packets until we get a video frame at or past target_pts
    for (stream, packet) in sess.ictx.packets() {
        if stream.index() == sess.video_stream_idx {
            sess.video_decoder.send_packet(&packet)?;
            let mut decoded = Video::empty();
            while sess.video_decoder.receive_frame(&mut decoded).is_ok() {
                // Compute PTS in seconds
                let tb = stream.time_base();
                let pts_secs = decoded.pts()
                    .unwrap_or(0) as f64 * tb.numerator() as f64
                    / tb.denominator() as f64;
                sess.last_pts = pts_secs;

                // Throttle to real-time: if we're ahead, sleep
                let ahead = pts_secs - target_pts;
                if ahead > 0.002 {
                    std::thread::sleep(Duration::from_secs_f64(ahead.min(0.05)));
                }

                // Scale frame to RGBA
                let mut rgb_frame = Video::empty();
                sess.scaler.run(&decoded, &mut rgb_frame)?;
                let w = rgb_frame.width();
                let h = rgb_frame.height();
                let data = rgb_frame.data(0).to_vec();

                {
                    let mut s = state.lock();
                    s.frame_rgba = Some((data, w, h));
                    s.position = pts_secs;
                }
                let _ = evt_tx.send(PlayerEvent::FrameReady);
                let _ = evt_tx.send(PlayerEvent::PositionChanged(pts_secs));
                return Ok(true);
            }
        }
    }

    Ok(false) // EOF
}
