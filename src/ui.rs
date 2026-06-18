// mediaplayer/src/ui.rs
// Renders the Windows-11-style dark media player UI using egui

use eframe::egui::{self, Color32, Pos2, Rect, Rounding, Stroke, Vec2};
use crate::app::MediaPlayerApp;
use crate::player::{PlayerCommand, PlayState};

// ── Palette ──────────────────────────────────────────────────────────────────
const BG_DARK: Color32       = Color32::from_rgb(18, 18, 20);
const BG_PANEL: Color32      = Color32::from_rgba_premultiplied(20, 20, 24, 220);
const ACCENT: Color32        = Color32::from_rgb(255, 165, 0);   // amber/orange
const ACCENT_DIM: Color32    = Color32::from_rgb(120, 80, 0);
const TEXT_PRIMARY: Color32  = Color32::from_rgb(240, 240, 242);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(160, 160, 168);
const ICON_COLOR: Color32    = Color32::from_rgb(220, 220, 228);
const SEEK_TRACK: Color32    = Color32::from_rgb(60, 60, 68);
const OVERLAY_BG: Color32    = Color32::from_rgba_premultiplied(0, 0, 0, 160);

pub fn draw(app: &mut MediaPlayerApp, ctx: &egui::Context, frame: &mut eframe::Frame) {
    apply_style(ctx);

    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(BG_DARK))
        .show(ctx, |ui| {
            let panel_rect = ui.max_rect();

            // ── 1. Video frame (full bleed) ───────────────────────────────
            draw_video(app, ui, panel_rect);

            // ── 2. Drag-and-drop hint when idle ──────────────────────────
            let is_idle = matches!(app.state.lock().state, PlayState::Idle);
            if is_idle {
                draw_drop_hint(ui, panel_rect);
            }

            // ── 3. Control bar overlay ────────────────────────────────────
            if app.show_controls {
                draw_control_bar(app, ui, panel_rect, ctx, frame);
            }
        });

    // Keyboard shortcuts
    handle_keys(app, ctx, frame);
}

fn draw_video(app: &mut MediaPlayerApp, ui: &mut egui::Ui, rect: Rect) {
    if let Some(ref tex) = app.video_texture {
        let tex_size = tex.size_vec2();
        let fitted = fit_rect(tex_size, rect);
        ui.painter().image(tex.id(), fitted, Rect::from_min_max(
            Pos2::ZERO, Pos2::new(1.0, 1.0),
        ), Color32::WHITE);
    }
}

fn draw_drop_hint(ui: &mut egui::Ui, rect: Rect) {
    let painter = ui.painter();
    // Centered drop hint
    let center = rect.center();

    // Circle icon
    painter.circle_stroke(center, 40.0, Stroke::new(2.0, Color32::from_rgb(80, 80, 90)));

    // Play triangle
    let tri_pts = [
        Pos2::new(center.x - 10.0, center.y - 16.0),
        Pos2::new(center.x - 10.0, center.y + 16.0),
        Pos2::new(center.x + 20.0, center.y),
    ];
    painter.add(egui::Shape::convex_polygon(
        tri_pts.to_vec(),
        Color32::from_rgb(80, 80, 90),
        Stroke::NONE,
    ));

    painter.text(
        Pos2::new(center.x, center.y + 65.0),
        egui::Align2::CENTER_CENTER,
        "Drop a video file to play",
        egui::FontId::proportional(18.0),
        TEXT_SECONDARY,
    );

    painter.text(
        Pos2::new(center.x, center.y + 92.0),
        egui::Align2::CENTER_CENTER,
        "MP4 · MKV · AVI · MOV · WMV · and more",
        egui::FontId::proportional(13.0),
        Color32::from_rgb(90, 90, 100),
    );
}

fn draw_control_bar(
    app: &mut MediaPlayerApp,
    ui: &mut egui::Ui,
    panel_rect: Rect,
    ctx: &egui::Context,
    frame: &mut eframe::Frame,
) {
    let bar_height = 90.0;
    let bar_rect = Rect::from_min_max(
        Pos2::new(panel_rect.left(), panel_rect.bottom() - bar_height),
        panel_rect.right_bottom(),
    );

    // Gradient overlay
    let painter = ui.painter();
    painter.rect_filled(bar_rect, Rounding::ZERO, OVERLAY_BG);

    let (pos, dur, vol, playing, shuffle, repeat) = {
        let s = app.state.lock();
        (s.position, s.duration, s.volume, 
         matches!(s.state, PlayState::Playing),
         s.shuffle, s.repeat)
    };

    // ── Seek bar ─────────────────────────────────────────────────────────────
    let seek_y = bar_rect.top() + 14.0;
    let seek_left = bar_rect.left() + 12.0;
    let seek_right = bar_rect.right() - 12.0;
    let seek_w = seek_right - seek_left;
    let seek_height = 4.0;

    let seek_rect = Rect::from_min_max(
        Pos2::new(seek_left, seek_y),
        Pos2::new(seek_right, seek_y + seek_height),
    );

    // Track background
    painter.rect_filled(seek_rect, Rounding::same(2.0), SEEK_TRACK);

    // Filled portion
    let fill_frac = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let filled_rect = Rect::from_min_max(
        seek_rect.min,
        Pos2::new(seek_rect.left() + seek_w * fill_frac, seek_rect.bottom()),
    );
    painter.rect_filled(filled_rect, Rounding::same(2.0), ACCENT);

    // Scrubber knob
    let knob_x = seek_rect.left() + seek_w * fill_frac;
    let knob_y = seek_y + seek_height / 2.0;
    painter.circle_filled(Pos2::new(knob_x, knob_y), 7.0, ACCENT);
    painter.circle_stroke(Pos2::new(knob_x, knob_y), 7.0, Stroke::new(1.5, Color32::WHITE));

    // Seek interaction
    let seek_interact_rect = Rect::from_min_max(
        Pos2::new(seek_left, seek_y - 10.0),
        Pos2::new(seek_right, seek_y + seek_height + 10.0),
    );
    let seek_resp = ui.interact(
        seek_interact_rect,
        egui::Id::new("seek_bar"),
        egui::Sense::click_and_drag(),
    );
    if seek_resp.is_pointer_button_down_on() || seek_resp.dragged() {
        if let Some(pos2) = seek_resp.interact_pointer_pos() {
            let frac = ((pos2.x - seek_left) / seek_w).clamp(0.0, 1.0) as f64;
            let new_pos = frac * dur;
            app.seek_value = new_pos;
            app.seeking = true;
            let _ = app.channels.cmd_tx.send(PlayerCommand::SeekTo(new_pos));
        }
    } else {
        app.seeking = false;
    }

    // ── Time labels ───────────────────────────────────────────────────────────
    let time_y = seek_y + 18.0;
    painter.text(
        Pos2::new(seek_left, time_y),
        egui::Align2::LEFT_TOP,
        format_time(pos),
        egui::FontId::proportional(12.0),
        TEXT_SECONDARY,
    );
    painter.text(
        Pos2::new(seek_right, time_y),
        egui::Align2::RIGHT_TOP,
        format_time(dur),
        egui::FontId::proportional(12.0),
        TEXT_SECONDARY,
    );

    // ── Bottom row ────────────────────────────────────────────────────────────
    let row_y = bar_rect.top() + 50.0;
    let center_x = bar_rect.center().x;

    // Thumbnail (bottom-left, 48×48)
    let thumb_size = 54.0;
    let thumb_rect = Rect::from_min_size(
        Pos2::new(bar_rect.left() + 12.0, row_y - 4.0),
        Vec2::splat(thumb_size),
    );
    if let Some(ref tex) = app.thumb_texture {
        painter.image(
            tex.id(),
            thumb_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        painter.rect_stroke(thumb_rect, Rounding::same(4.0), Stroke::new(1.0, Color32::from_rgb(60, 60, 70)));
    } else {
        painter.rect_filled(thumb_rect, Rounding::same(4.0), Color32::from_rgb(35, 35, 40));
    }

    // File name under thumbnail area
    let file_name = app.state.lock().current_file
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    if !file_name.is_empty() {
        painter.text(
            Pos2::new(bar_rect.left() + 12.0 + thumb_size + 10.0, row_y + 4.0),
            egui::Align2::LEFT_TOP,
            truncate_str(&file_name, 28),
            egui::FontId::proportional(13.0),
            TEXT_PRIMARY,
        );
    }

    // ── Center transport controls ─────────────────────────────────────────────
    let btn_spacing = 40.0;
    let btn_y = row_y + 14.0;

    // Shuffle
    let shuffle_col = if shuffle { ACCENT } else { ICON_COLOR };
    if icon_button(ui, painter, "⇄", Pos2::new(center_x - btn_spacing * 2.5, btn_y), 14.0, shuffle_col) {
        let mut s = app.state.lock();
        s.shuffle = !s.shuffle;
    }

    // Skip back 10s
    if icon_button(ui, painter, "⏮", Pos2::new(center_x - btn_spacing * 1.5, btn_y), 16.0, ICON_COLOR) {
        let _ = app.channels.cmd_tx.send(PlayerCommand::SkipBack(10.0));
    }

    // Play/Pause (big)
    let play_pos = Pos2::new(center_x, btn_y);
    let play_icon = if playing { "⏸" } else { "▶" };
    painter.circle_filled(play_pos, 20.0, Color32::from_rgb(255, 255, 255));
    painter.text(
        play_pos,
        egui::Align2::CENTER_CENTER,
        play_icon,
        egui::FontId::proportional(16.0),
        Color32::BLACK,
    );
    let play_resp = ui.interact(
        Rect::from_center_size(play_pos, Vec2::splat(40.0)),
        egui::Id::new("play_pause_btn"),
        egui::Sense::click(),
    );
    if play_resp.clicked() {
        if playing {
            let _ = app.channels.cmd_tx.send(PlayerCommand::Pause);
        } else {
            let _ = app.channels.cmd_tx.send(PlayerCommand::Play);
        }
    }

    // Skip forward 10s
    if icon_button(ui, painter, "⏭", Pos2::new(center_x + btn_spacing * 1.5, btn_y), 16.0, ICON_COLOR) {
        let _ = app.channels.cmd_tx.send(PlayerCommand::SkipForward(10.0));
    }

    // Repeat
    let repeat_col = if repeat { ACCENT } else { ICON_COLOR };
    if icon_button(ui, painter, "↺", Pos2::new(center_x + btn_spacing * 2.5, btn_y), 16.0, repeat_col) {
        let mut s = app.state.lock();
        s.repeat = !s.repeat;
    }

    // ── Right side: Volume + Fullscreen ──────────────────────────────────────
    let right_x = bar_rect.right() - 12.0;

    // Fullscreen toggle
    if icon_button(ui, painter, "⛶", Pos2::new(right_x - 24.0, btn_y), 16.0, ICON_COLOR) {
        app.is_fullscreen = !app.is_fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.is_fullscreen));
    }

    // Volume icon + slider
    let vol_icon = if vol == 0.0 { "🔇" } else if vol < 0.5 { "🔉" } else { "🔊" };
    painter.text(
        Pos2::new(right_x - 140.0, btn_y),
        egui::Align2::CENTER_CENTER,
        vol_icon,
        egui::FontId::proportional(14.0),
        ICON_COLOR,
    );

    // Volume bar (80px wide)
    let vol_left = right_x - 125.0;
    let vol_right = right_x - 56.0;
    let vol_track = Rect::from_min_max(
        Pos2::new(vol_left, btn_y - 2.0),
        Pos2::new(vol_right, btn_y + 2.0),
    );
    painter.rect_filled(vol_track, Rounding::same(2.0), SEEK_TRACK);
    let vol_fill = Rect::from_min_max(
        vol_track.min,
        Pos2::new(vol_left + (vol_right - vol_left) * vol, vol_track.bottom()),
    );
    painter.rect_filled(vol_fill, Rounding::same(2.0), ACCENT);
    painter.circle_filled(
        Pos2::new(vol_left + (vol_right - vol_left) * vol, btn_y),
        5.0,
        ACCENT,
    );

    let vol_resp = ui.interact(vol_track.expand(8.0), egui::Id::new("vol_slider"), egui::Sense::click_and_drag());
    if vol_resp.is_pointer_button_down_on() || vol_resp.dragged() {
        if let Some(p) = vol_resp.interact_pointer_pos() {
            let new_vol = ((p.x - vol_left) / (vol_right - vol_left)).clamp(0.0, 1.0);
            app.state.lock().volume = new_vol;
            let _ = app.channels.cmd_tx.send(PlayerCommand::SetVolume(new_vol));
        }
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    icon: &str,
    center: Pos2,
    size: f32,
    color: Color32,
) -> bool {
    let rect = Rect::from_center_size(center, Vec2::splat(size + 12.0));
    let resp = ui.interact(rect, egui::Id::new(icon).with(center.x as i32), egui::Sense::click());
    let col = if resp.hovered() { Color32::WHITE } else { color };
    painter.text(center, egui::Align2::CENTER_CENTER, icon, egui::FontId::proportional(size), col);
    resp.clicked()
}

fn handle_keys(app: &mut MediaPlayerApp, ctx: &egui::Context, frame: &mut eframe::Frame) {
    ctx.input(|i| {
        use egui::Key;
        if i.key_pressed(Key::Space) {
            let playing = matches!(app.state.lock().state, PlayState::Playing);
            let cmd = if playing { PlayerCommand::Pause } else { PlayerCommand::Play };
            let _ = app.channels.cmd_tx.send(cmd);
        }
        if i.key_pressed(Key::ArrowRight) {
            let _ = app.channels.cmd_tx.send(PlayerCommand::SkipForward(10.0));
        }
        if i.key_pressed(Key::ArrowLeft) {
            let _ = app.channels.cmd_tx.send(PlayerCommand::SkipBack(10.0));
        }
        if i.key_pressed(Key::ArrowUp) {
            let vol = (app.state.lock().volume + 0.05).min(1.0);
            app.state.lock().volume = vol;
            let _ = app.channels.cmd_tx.send(PlayerCommand::SetVolume(vol));
        }
        if i.key_pressed(Key::ArrowDown) {
            let vol = (app.state.lock().volume - 0.05).max(0.0);
            app.state.lock().volume = vol;
            let _ = app.channels.cmd_tx.send(PlayerCommand::SetVolume(vol));
        }
        if i.key_pressed(Key::F) {
            app.is_fullscreen = !app.is_fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.is_fullscreen));
        }
        if i.key_pressed(Key::Escape) && app.is_fullscreen {
            app.is_fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
        if i.key_pressed(Key::N) {
            app.play_next();
        }
        if i.key_pressed(Key::P) {
            app.play_prev();
        }
    });
    let _ = frame;
}

fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.window_fill = BG_DARK;
    style.visuals.panel_fill = BG_DARK;
    style.visuals.override_text_color = Some(TEXT_PRIMARY);
    ctx.set_style(style);
}

fn format_time(secs: f64) -> String {
    let s = secs as u64;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let s = s % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_chars - 1).collect::<String>())
    }
}

fn fit_rect(content: Vec2, container: Rect) -> Rect {
    let cw = container.width();
    let ch = container.height();
    let scale = (cw / content.x).min(ch / content.y);
    let w = content.x * scale;
    let h = content.y * scale;
    let x = container.left() + (cw - w) / 2.0;
    let y = container.top() + (ch - h) / 2.0;
    Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h))
}
