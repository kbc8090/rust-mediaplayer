// mediaplayer/src/app.rs
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
    pub state:   Arc<Mutex<PlaybackState>>,
    pub channels: PlayerChannels,

    pub video_texture: Option<TextureHandle>,
    pub thumb_texture: Option<TextureHandle>,

    pub show_controls:       bool,
    pub controls_hide_timer: f32,
    pub seeking:             bool,
    pub seek_value:          f64,
    pub is_fullscreen:       bool,
}

impl MediaPlayerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (channels, state) = spawn_player();
        Self {
            state,
            channels,
            video_texture:       None,
            thumb_texture:       None,
            show_controls:       true,
            controls_hide_timer: 0.0,
            seeking:             false,
            seek_value:          0.0,
            is_fullscreen:       false,
        }
    }

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
        let path = {
            let mut s = self.state.lock();
            if s.shuffle {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut h = DefaultHasher::new();
                std::time::SystemTime::now().hash(&mut h);
                s.playlist_index = (h.finish() as usize) % s.playlist.len().max(1);
            } else if s.playlist_index + 1 < s.playlist.len() {
                s.playlist_index += 1;
            } else if s.repeat {
                s.playlist_index = 0;
            } else {
                return;
            }
            s.playlist[s.playlist_index].clone()
        };
        let _ = self.channels.cmd_tx.send(PlayerCommand::Open(path));
    }

    pub fn play_prev(&mut self) {
        let path = {
            let mut s = self.state.lock();
            if s.playlist_index > 0 { s.playlist_index -= 1; }
            s.playlist[s.playlist_index].clone()
        };
        let _ = self.channels.cmd_tx.send(PlayerCommand::Open(path));
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        while let Ok(evt) = self.channels.evt_rx.try_recv() {
            match evt {
                PlayerEvent::FrameReady => {
                    if let Some((data, w, h)) = self.state.lock().frame_rgba.clone() {
                        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &data);
                        match &mut self.video_texture {
                            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                            None    => self.video_texture = Some(ctx.load_texture("video_frame", img, egui::TextureOptions::LINEAR)),
                        }
                    }
                    if self.thumb_texture.is_none() {
                        if let Some((data, w, h)) = self.state.lock().thumbnail_rgba.clone() {
                            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &data);
                            self.thumb_texture = Some(ctx.load_texture("thumbnail", img, egui::TextureOptions::LINEAR));
                        }
                    }
                    ctx.request_repaint();
                }
                PlayerEvent::Ended => self.play_next(),
                _ => {}
            }
        }
    }
}

impl eframe::App for MediaPlayerApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll_events(ctx);

        // Drag-and-drop
        let dropped: Vec<PathBuf> = ctx.input(|i|
            i.raw.dropped_files.iter()
                .filter_map(|f| f.path.clone())
                .filter(|p| is_video_file(p))
                .collect()
        );
        if !dropped.is_empty() { self.open_files(dropped); }

        // Auto-hide controls in fullscreen after 3s
        let mouse_moved = ctx.input(|i| i.pointer.velocity().length() > 1.0);
        if mouse_moved {
            self.controls_hide_timer = 0.0;
            self.show_controls = true;
        } else if self.is_fullscreen && matches!(self.state.lock().state, PlayState::Playing) {
            self.controls_hide_timer += ctx.input(|i| i.stable_dt);
            if self.controls_hide_timer > 3.0 { self.show_controls = false; }
        }

        ui::draw(self, ctx, frame);

        if matches!(self.state.lock().state, PlayState::Playing) {
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
    }
}

pub fn is_video_file(path: &PathBuf) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase()).as_deref(),
        Some("mp4"|"mkv"|"avi"|"mov"|"wmv"|"flv"|"webm"|"m4v"|"ts"|"m2ts"|"mpg"|"mpeg"|"3gp"|"ogv")
    )
}
