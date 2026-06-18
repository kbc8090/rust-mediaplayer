// mediaplayer/src/ui.rs
use eframe::egui::{self, Color32, Pos2, Rect, Rounding, Stroke, Vec2};
use crate::app::MediaPlayerApp;
use crate::player::{PlayerCommand, PlayState};

const BG_DARK:        Color32 = Color32::from_rgb(18, 18, 20);
const ACCENT:         Color32 = Color32::from_rgb(255, 165, 0);
const TEXT_PRIMARY:   Color32 = Color32::from_rgb(240, 240, 242);
const TEXT_SECONDARY: Color32 = Color32::from_rgb(160, 160, 168);
const ICON_COLOR:     Color32 = Color32::from_rgb(220, 220, 228);
const SEEK_TRACK:     Color32 = Color32::from_rgb(60, 60, 68);
const OVERLAY_BG:     Color32 = Color32::from_rgba_premultiplied(0, 0, 0, 160);

pub fn draw(app: &mut MediaPlayerApp, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    apply_style(ctx);

    egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(BG_DARK))
        .show(ctx, |ui| {
            let panel_rect = ui.max_rect();
            draw_video(app, ui, panel_rect);

            let is_idle = matches!(app.state.lock().state, PlayState::Idle);
            if is_idle {
                draw_drop_hint(ui, panel_rect);
            }

            if app.show_controls {
                draw_control_bar(app, ui, panel_rect, ctx);
            }
        });

    handle_keys(app, ctx);
}

fn draw_video(app: &mut MediaPlayerApp, ui: &mut egui::Ui, rect: Rect) {
    if let Some(ref tex) = app.video_texture {
        let fitted = fit_rect(tex.size_vec2(), rect);
        ui.painter().image(
            tex.id(),
            fitted,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

fn draw_drop_hint(ui: &mut egui::Ui, rect: Rect) {
    let center = rect.center();
    let painter = ui.painter();
    painter.circle_stroke(center, 40.0, Stroke::new(2.0, Color32::from_rgb(80, 80, 90)));
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(center.x - 10.0, center.y - 16.0),
            Pos2::new(center.x - 10.0, center.y + 16.0),
            Pos2::new(center.x + 20.0, center.y),
        ],
        Color32::from_rgb(80, 80, 90),
        Stroke::NONE,
    ));
    painter.text(Pos2::new(center.x, center.y + 65.0), egui::Align2::CENTER_CENTER,
        "Drop a video file to play", egui::FontId::proportional(18.0), TEXT_SECONDARY);
    painter.text(Pos2::new(center.x, center.y + 92.0), egui::Align2::CENTER_CENTER,
        "MP4 · MKV · AVI · MOV · WMV · and more", egui::FontId::proportional(13.0),
        Color32::from_rgb(90, 90, 100));
}

fn draw_control_bar(
    app: &mut MediaPlayerApp,
    ui:  &mut egui::Ui,
    panel_rect: Rect,
    ctx: &egui::Context,
) {
    let bar_height  = 90.0;
    let bar_rect    = Rect::from_min_max(
        Pos2::new(panel_rect.left(), panel_rect.bottom() - bar_height),
        panel_rect.right_bottom(),
    );

    let (pos, dur, _vol, playing, shuffle, repeat) = {
        let s = app.state.lock();
        (s.position, s.duration, s.volume,
         matches!(s.state, PlayState::Playing), s.shuffle, s.repeat)
    };

    // ── Layout ────────────────────────────────────────────────────────────────
    let seek_y      = bar_rect.top() + 14.0;
    let seek_left   = bar_rect.left() + 12.0;
    let seek_right  = bar_rect.right() - 12.0;
    let seek_w      = seek_right - seek_left;
    let row_y       = bar_rect.top() + 50.0;
    let center_x    = bar_rect.center().x;
    let btn_y       = row_y + 14.0;
    let btn_gap     = 40.0;
    let right_x     = bar_rect.right() - 12.0;
    let thumb_size  = 54.0;
    let fill_frac   = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let knob_x      = seek_left + seek_w * fill_frac;
    let vol_left    = right_x - 125.0;
    let vol_right   = right_x - 56.0;

    let seek_rect  = Rect::from_min_max(Pos2::new(seek_left, seek_y),   Pos2::new(seek_right, seek_y + 4.0));
    let vol_track  = Rect::from_min_max(Pos2::new(vol_left, btn_y - 2.0), Pos2::new(vol_right, btn_y + 2.0));
    let thumb_rect = Rect::from_min_size(Pos2::new(bar_rect.left() + 12.0, row_y - 4.0), Vec2::splat(thumb_size));

    // ── Interactions (all mutable ui borrows before any painter borrow) ───────
    let btn = |id: &str, cx: f32, w: f32| -> egui::Response {
        ui.interact(
            Rect::from_center_size(Pos2::new(cx, btn_y), Vec2::splat(w)),
            egui::Id::new(id),
            egui::Sense::click(),
        )
    };
    let seek_resp    = ui.interact(seek_rect.expand(10.0),  egui::Id::new("seek"),    egui::Sense::click_and_drag());
    let play_resp    = btn("play",    center_x,                    40.0);
    let shuffle_resp = btn("shuffle", center_x - btn_gap * 2.5,   26.0);
    let skipb_resp   = btn("skipb",   center_x - btn_gap * 1.5,   28.0);
    let skipf_resp   = btn("skipf",   center_x + btn_gap * 1.5,   28.0);
    let repeat_resp  = btn("repeat",  center_x + btn_gap * 2.5,   26.0);
    let fs_resp      = btn("fs",      right_x - 24.0,              28.0);
    let vol_resp     = ui.interact(vol_track.expand(8.0),   egui::Id::new("vol"),     egui::Sense::click_and_drag());

    // ── Handle events ─────────────────────────────────────────────────────────
    if seek_resp.is_pointer_button_down_on() || seek_resp.dragged() {
        if let Some(p) = seek_resp.interact_pointer_pos() {
            let frac = ((p.x - seek_left) / seek_w).clamp(0.0, 1.0) as f64;
            app.seek_value = frac * dur;
            app.seeking = true;
            let _ = app.channels.cmd_tx.send(PlayerCommand::SeekTo(app.seek_value));
        }
    } else { app.seeking = false; }

    if play_resp.clicked()    { let _ = app.channels.cmd_tx.send(if playing { PlayerCommand::Pause } else { PlayerCommand::Play }); }
    if shuffle_resp.clicked() { app.state.lock().shuffle = !shuffle; }
    if skipb_resp.clicked()   { let _ = app.channels.cmd_tx.send(PlayerCommand::SkipBack(10.0)); }
    if skipf_resp.clicked()   { let _ = app.channels.cmd_tx.send(PlayerCommand::SkipForward(10.0)); }
    if repeat_resp.clicked()  { app.state.lock().repeat = !repeat; }
    if fs_resp.clicked()      { app.is_fullscreen = !app.is_fullscreen; ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.is_fullscreen)); }
    if vol_resp.is_pointer_button_down_on() || vol_resp.dragged() {
        if let Some(p) = vol_resp.interact_pointer_pos() {
            let v = ((p.x - vol_left) / (vol_right - vol_left)).clamp(0.0, 1.0);
            app.state.lock().volume = v;
            let _ = app.channels.cmd_tx.send(PlayerCommand::SetVolume(v));
        }
    }

    // ── Paint (all immutable painter borrows after all ui.interact calls) ─────
    let painter = ui.painter();

    painter.rect_filled(bar_rect, Rounding::ZERO, OVERLAY_BG);

    // Seek bar
    painter.rect_filled(seek_rect, Rounding::same(2.0), SEEK_TRACK);
    if fill_frac > 0.0 {
        painter.rect_filled(
            Rect::from_min_max(seek_rect.min, Pos2::new(seek_left + seek_w * fill_frac, seek_rect.bottom())),
            Rounding::same(2.0), ACCENT,
        );
    }
    painter.circle_filled(Pos2::new(knob_x, seek_y + 2.0), 7.0, ACCENT);
    painter.circle_stroke( Pos2::new(knob_x, seek_y + 2.0), 7.0, Stroke::new(1.5, Color32::WHITE));

    // Time labels
    painter.text(Pos2::new(seek_left,  seek_y + 18.0), egui::Align2::LEFT_TOP,  format_time(pos), egui::FontId::proportional(12.0), TEXT_SECONDARY);
    painter.text(Pos2::new(seek_right, seek_y + 18.0), egui::Align2::RIGHT_TOP, format_time(dur), egui::FontId::proportional(12.0), TEXT_SECONDARY);

    // Thumbnail
    if let Some(ref tex) = app.thumb_texture {
        painter.image(tex.id(), thumb_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        painter.rect_stroke(thumb_rect, Rounding::same(4.0), Stroke::new(1.0, Color32::from_rgb(60,60,70)));
    } else {
        painter.rect_filled(thumb_rect, Rounding::same(4.0), Color32::from_rgb(35,35,40));
    }

    // Filename
    let file_name = app.state.lock().current_file
        .as_ref().and_then(|p| p.file_stem()).and_then(|s| s.to_str()).unwrap_or("").to_string();
    if !file_name.is_empty() {
        painter.text(
            Pos2::new(bar_rect.left() + 12.0 + thumb_size + 10.0, row_y + 4.0),
            egui::Align2::LEFT_TOP, truncate_str(&file_name, 28),
            egui::FontId::proportional(13.0), TEXT_PRIMARY,
        );
    }

    // Transport buttons
    let ic = |r: &egui::Response, base: Color32| if r.hovered() { Color32::WHITE } else { base };
    painter.text(Pos2::new(center_x - btn_gap*2.5, btn_y), egui::Align2::CENTER_CENTER, "⇄", egui::FontId::proportional(14.0), ic(&shuffle_resp, if shuffle { ACCENT } else { ICON_COLOR }));
    painter.text(Pos2::new(center_x - btn_gap*1.5, btn_y), egui::Align2::CENTER_CENTER, "⏮", egui::FontId::proportional(16.0), ic(&skipb_resp,   ICON_COLOR));
    painter.circle_filled(Pos2::new(center_x, btn_y), 20.0, Color32::WHITE);
    painter.text(Pos2::new(center_x, btn_y), egui::Align2::CENTER_CENTER, if playing {"⏸"} else {"▶"}, egui::FontId::proportional(16.0), Color32::BLACK);
    painter.text(Pos2::new(center_x + btn_gap*1.5, btn_y), egui::Align2::CENTER_CENTER, "⏭", egui::FontId::proportional(16.0), ic(&skipf_resp,  ICON_COLOR));
    painter.text(Pos2::new(center_x + btn_gap*2.5, btn_y), egui::Align2::CENTER_CENTER, "↺", egui::FontId::proportional(16.0), ic(&repeat_resp,  if repeat { ACCENT } else { ICON_COLOR }));
    painter.text(Pos2::new(right_x - 24.0, btn_y),          egui::Align2::CENTER_CENTER, "⛶", egui::FontId::proportional(16.0), ic(&fs_resp,     ICON_COLOR));

    // Volume
    let vol_cur = app.state.lock().volume;
    let vol_icon = if vol_cur == 0.0 {"🔇"} else if vol_cur < 0.5 {"🔉"} else {"🔊"};
    painter.text(Pos2::new(right_x - 140.0, btn_y), egui::Align2::CENTER_CENTER, vol_icon, egui::FontId::proportional(14.0), ICON_COLOR);
    painter.rect_filled(vol_track, Rounding::same(2.0), SEEK_TRACK);
    painter.rect_filled(Rect::from_min_max(vol_track.min, Pos2::new(vol_left + (vol_right-vol_left)*vol_cur, vol_track.bottom())), Rounding::same(2.0), ACCENT);
    painter.circle_filled(Pos2::new(vol_left + (vol_right-vol_left)*vol_cur, btn_y), 5.0, ACCENT);
}

fn handle_keys(app: &mut MediaPlayerApp, ctx: &egui::Context) {
    ctx.input(|i| {
        use egui::Key;
        if i.key_pressed(Key::Space) {
            let cmd = if matches!(app.state.lock().state, PlayState::Playing) { PlayerCommand::Pause } else { PlayerCommand::Play };
            let _ = app.channels.cmd_tx.send(cmd);
        }
        if i.key_pressed(Key::ArrowRight) { let _ = app.channels.cmd_tx.send(PlayerCommand::SkipForward(10.0)); }
        if i.key_pressed(Key::ArrowLeft)  { let _ = app.channels.cmd_tx.send(PlayerCommand::SkipBack(10.0)); }
        if i.key_pressed(Key::ArrowUp)   { let v = (app.state.lock().volume + 0.05).min(1.0); app.state.lock().volume = v; let _ = app.channels.cmd_tx.send(PlayerCommand::SetVolume(v)); }
        if i.key_pressed(Key::ArrowDown) { let v = (app.state.lock().volume - 0.05).max(0.0); app.state.lock().volume = v; let _ = app.channels.cmd_tx.send(PlayerCommand::SetVolume(v)); }
        if i.key_pressed(Key::F) { app.is_fullscreen = !app.is_fullscreen; ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.is_fullscreen)); }
        if i.key_pressed(Key::Escape) && app.is_fullscreen { app.is_fullscreen = false; ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false)); }
        if i.key_pressed(Key::N) { app.play_next(); }
        if i.key_pressed(Key::P) { app.play_prev(); }
    });
}

fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.window_fill = BG_DARK;
    style.visuals.panel_fill  = BG_DARK;
    style.visuals.override_text_color = Some(TEXT_PRIMARY);
    ctx.set_style(style);
}

fn format_time(secs: f64) -> String {
    let s = secs as u64;
    let (h, m, s) = (s/3600, (s%3600)/60, s%60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn truncate_str(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() }
    else { format!("{}…", s.chars().take(n-1).collect::<String>()) }
}

fn fit_rect(content: Vec2, container: Rect) -> Rect {
    let scale = (container.width() / content.x).min(container.height() / content.y);
    let size  = content * scale;
    Rect::from_center_size(container.center(), size)
}
