// mediaplayer/src/app.rs
// Top-level eframe App — wires UI, player channels, and shared state

use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::Mutex;
use eframe::egui;
use eframe::egui::TextureHandle;

use crate::player::{
    spawn_player, PlaybackState, PlayerChannels, PlayerCommand, PlayerEvent, PlayState,
};
use crate::ui;

pub struct MediaPlayerApp {
    pub state: Arc<Mutex<PlaybackState>>,
    pub channels: PlayerChannels,

    // egui texture for the current video frame
    pub video_texture: Option<TextureHandle>,
    pub thumb_texture: Option<TextureHandle>,

    // UI state
    pub show_controls: bool,
    pub controls_hide_timer: f32, // seconds since last mouse move
    pub seeking: bool,
    pub seek_value: f64,
    pub is_fullscreen: bool,
}

impl MediaPlayerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (channels, state) = spawn_player();
        Self {
            state,
            channels,
            video_texture: None,
            thumb_texture: None,
            show_controls: true,
            controls_hide_timer: 0.0,
            seeking: false,
            seek_value: 0.0,
            is_fullscreen: false,
        }
    }

    /// Open a file: add to playlist, send Open command
    pub fn open_file(&mut self, path: PathBuf) {
        {
            let mut s = self.state.lock();
            s.playlist.clear();
            s.playlist.push(path.clone());
            s.playlist_index = 0;
        }
        let _ = self.channels.cmd_tx.send(PlayerCommand::Open(path));
    }

    /// Open multiple files as a playlist
    pub fn open_files(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() { return; }
        let first = paths[0].clone();
        {
            let mut s = self.state.lock();
            s.playlist = paths;
            s.playlist_index = 0;
        }
        let _ = self.channels.cmd_tx.send(PlayerCommand::Open(first));
    }

    pub fn play_next(&mut self) {
        let (path, idx) = {
            let mut s = self.state.lock();
            if s.shuffle {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                std::time::SystemTime::now().hash(&mut h);
                let i = (h.finish() as usize) % s.playlist.len();
                s.playlist_index = i;
            } else if s.playlist_index + 1 < s.playlist.len() {
                s.playlist_index += 1;
            } else if s.repeat {
                s.playlist_index = 0;
            } else {
                return;
            }
            (s.playlist[s.playlist_index].clone(), s.playlist_index)
        };
        let _ = self.channels.cmd_tx.send(PlayerCommand::Open(path));
        let _ = idx;
    }

    pub fn play_prev(&mut self) {
        let path = {
            let mut s = self.state.lock();
            if s.playlist_index > 0 {
                s.playlist_index -= 1;
            }
            s.playlist[s.playlist_index].clone()
        };
        let _ = self.channels.cmd_tx.send(PlayerCommand::Open(path));
    }

    /// Poll events from the player thread
    fn poll_events(&mut self, ctx: &egui::Context) {
        while let Ok(evt) = self.channels.evt_rx.try_recv() {
            match evt {
                PlayerEvent::FrameReady => {
                    // Upload new frame to GPU texture
                    if let Some((data, w, h)) = self.state.lock().frame_rgba.clone() {
                        let color_image = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            &data,
                        );
                        match &mut self.video_texture {
                            Some(tex) => tex.set(color_image, egui::TextureOptions::LINEAR),
                            None => {
                                self.video_texture = Some(ctx.load_texture(
                                    "video_frame",
                                    color_image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                        }
                    }
                    // Upload thumbnail once
                    if self.thumb_texture.is_none() {
                        if let Some((data, w, h)) = self.state.lock().thumbnail_rgba.clone() {
                            let img = egui::ColorImage::from_rgba_unmultiplied(
                                [w as usize, h as usize],
                                &data,
                            );
                            self.thumb_texture = Some(ctx.load_texture(
                                "thumbnail",
                                img,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    }
                    ctx.request_repaint();
                }
                PlayerEvent::Ended => {
                    self.play_next();
                }
                _ => {}
            }
        }
    }
}

impl eframe::App for MediaPlayerApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll_events(ctx);

        // Handle drag-and-drop
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .filter(|p| is_video_file(p))
                .collect()
        });
        if !dropped.is_empty() {
            self.open_files(dropped);
        }

        // Fullscreen sync
        if self.is_fullscreen != frame.info().window_info.fullscreen {
            // keep in sync if user pressed OS key
            self.is_fullscreen = frame.info().window_info.fullscreen;
        }

        // Auto-hide controls after 3s of no mouse movement
        let mouse_moved = ctx.input(|i| i.pointer.velocity().length() > 1.0);
        if mouse_moved {
            self.controls_hide_timer = 0.0;
            self.show_controls = true;
        } else {
            let dt = ctx.input(|i| i.stable_dt);
            let playing = matches!(self.state.lock().state, PlayState::Playing);
            if playing && self.is_fullscreen {
                self.controls_hide_timer += dt;
                if self.controls_hide_timer > 3.0 {
                    self.show_controls = false;
                }
            }
        }

        ui::draw(self, ctx, frame);

        // Keep repainting while playing
        if matches!(self.state.lock().state, PlayState::Playing) {
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
    }
}

pub fn is_video_file(path: &PathBuf) -> bool {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => matches!(
            ext.to_lowercase().as_str(),
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm"
                | "m4v" | "ts" | "m2ts" | "mpg" | "mpeg" | "3gp" | "ogv"
        ),
        None => false,
    }
}
