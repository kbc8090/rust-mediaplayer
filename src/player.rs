// mediaplayer/src/player.rs
// Core playback state machine

use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::Mutex;
use crossbeam_channel::{Sender, Receiver, bounded};

/// Commands sent from UI → player thread
#[derive(Debug, Clone)]
pub enum PlayerCommand {
    Open(PathBuf),
    Play,
    Pause,
    SeekTo(f64),       // seconds
    SetVolume(f32),    // 0.0 – 1.0
    SkipForward(f64),  // seconds
    SkipBack(f64),
    Next,
    Prev,
    Stop,
}

/// Events sent from player thread → UI
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Opened { duration: f64, path: PathBuf },
    PositionChanged(f64),
    Paused,
    Playing,
    Ended,
    Error(String),
    FrameReady,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayState {
    Idle,
    Playing,
    Paused,
    Ended,
    Error(String),
}

/// Shared playback state (Arc<Mutex<PlaybackState>>)
pub struct PlaybackState {
    pub state: PlayState,
    pub position: f64,
    pub duration: f64,
    pub volume: f32,
    pub muted: bool,
    pub shuffle: bool,
    pub repeat: bool,
    pub current_file: Option<PathBuf>,
    pub playlist: Vec<PathBuf>,
    pub playlist_index: usize,
    /// Latest decoded RGBA video frame for display
    pub frame_rgba: Option<(Vec<u8>, u32, u32)>,  // (data, width, height)
    /// Thumbnail (first frame) for the bottom-left thumbnail
    pub thumbnail_rgba: Option<(Vec<u8>, u32, u32)>,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            state: PlayState::Idle,
            position: 0.0,
            duration: 0.0,
            volume: 0.8,
            muted: false,
            shuffle: false,
            repeat: false,
            current_file: None,
            playlist: Vec::new(),
            playlist_index: 0,
            frame_rgba: None,
            thumbnail_rgba: None,
        }
    }
}

/// Channel handles passed to the app
pub struct PlayerChannels {
    pub cmd_tx: Sender<PlayerCommand>,
    pub evt_rx: Receiver<PlayerEvent>,
}

/// Spawn the player background thread, return channels + shared state
pub fn spawn_player() -> (PlayerChannels, Arc<Mutex<PlaybackState>>) {
    let (cmd_tx, cmd_rx) = bounded::<PlayerCommand>(64);
    let (evt_tx, evt_rx) = bounded::<PlayerEvent>(256);
    let state = Arc::new(Mutex::new(PlaybackState::default()));
    let state_clone = Arc::clone(&state);

    std::thread::spawn(move || {
        crate::decoder::player_thread(cmd_rx, evt_tx, state_clone);
    });

    (PlayerChannels { cmd_tx, evt_rx }, state)
}
