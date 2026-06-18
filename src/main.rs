#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams, RenderParam, RenderParamApiType}};
use std::sync::{Arc, Mutex};
use std::cell::RefCell;

unsafe extern "system" {
    fn wglGetProcAddress(lpszProc: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(lpModuleName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}

thread_local! {
    static TLS_RENDER_CTX: RefCell<Option<RenderContext<'static>>> = const { RefCell::new(None) };
}

// ── Palette ───────────────────────────────────────────────────────────────────
const ACCENT:   egui::Color32 = egui::Color32::from_rgb(255, 165, 0);
const BAR_BG:   egui::Color32 = egui::Color32::from_rgba_premultiplied(14, 14, 18, 240);
const TRACK_BG: egui::Color32 = egui::Color32::from_rgb(55, 55, 65);
const ICON_DIM: egui::Color32 = egui::Color32::from_rgb(200, 200, 210);
const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(150, 150, 160);

// Controls bar height — compact
const BAR_H: f32 = 52.0;
// Bottom trigger zone for showing controls in fullscreen (px from bottom of screen)
const TRIGGER_ZONE: f32 = 20.0;

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    render_ctx_initialized: bool,
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
    time_pos: f64,
    duration: f64,
    volume: f64,
    show_controls: bool,
    /// Wall-clock instant of last mouse movement, used for auto-hide
    last_mouse_move: std::time::Instant,
    has_file: bool,
    /// Wakeup sender so mpv can ask for repaints from its own thread
    repaint_tx: std::sync::mpsc::SyncSender<()>,
}

impl FluentMediaPlayer {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mpv = Mpv::new().expect("Failed to initialize libmpv!");
        let _ = mpv.set_property("hwdec", "auto");
        let _ = mpv.set_property("osc", "no");
        let _ = mpv.set_property("keep-open", "yes");
        let _ = mpv.set_property("volume", 80_i64);
        // Let mpv decide its own frame pacing — do NOT tell egui to repaint every frame.
        // Instead we use a channel: mpv signals us when a new frame is ready.
        let _ = mpv.set_property("video-sync", "display-resample");

        // Channel: mpv render thread → egui repaint
        let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
        let ctx_clone = cc.egui_ctx.clone();
        // Spawn a thread that wakes egui whenever mpv signals a new frame
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                ctx_clone.request_repaint();
            }
        });

        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            render_ctx_initialized: false,
            current_file: String::new(),
            is_playing: false,
            is_fullscreen: false,
            time_pos: 0.0,
            duration: 0.0,
            volume: 80.0,
            show_controls: true,
            last_mouse_move: std::time::Instant::now(),
            has_file: false,
            repaint_tx: tx,
        }
    }

    fn open_file(&mut self, path: &str) {
        self.current_file = path.to_owned();
        self.has_file = true;
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.command("loadfile", &[path]);
            let _ = mpv.set_property("pause", false);
        }
        self.is_playing = true;
    }

    fn toggle_play_pause(&mut self) {
        self.is_playing = !self.is_playing;
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.set_property("pause", !self.is_playing);
        }
    }

    fn seek_to(&mut self, secs: f64) {
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.set_property("time-pos", secs);
        }
    }

    fn set_volume(&mut self, vol: f64) {
        self.volume = vol.clamp(0.0, 100.0);
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.set_property("volume", self.volume);
        }
    }

    fn skip(&mut self, secs: f64) {
        let new_pos = (self.time_pos + secs).clamp(0.0, self.duration);
        self.seek_to(new_pos);
    }

    fn file_stem(&self) -> &str {
        std::path::Path::new(&self.current_file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // ── Poll mpv properties (cheap IPC calls) ─────────────────────────────
        if let Ok(mpv) = self.mpv.lock() {
            if let Ok(t) = mpv.get_property::<f64>("time-pos") { self.time_pos = t; }
            if let Ok(d) = mpv.get_property::<f64>("duration") { self.duration = d; }
            if let Ok(v) = mpv.get_property::<f64>("volume")   { self.volume   = v; }
            if let Ok(p) = mpv.get_property::<bool>("pause")   { self.is_playing = !p; }
        }

        // ── Initialize mpv OpenGL render context once GL is ready ─────────────
        if !self.render_ctx_initialized && frame.gl().is_some() {
            let init_params = OpenGLInitParams {
                get_proc_address: |_: &*mut std::os::raw::c_void, name| unsafe {
                    let c_name = std::ffi::CString::new(name).unwrap();
                    let addr = wglGetProcAddress(c_name.as_ptr());
                    if !addr.is_null() && (addr as usize) > 3 && (addr as usize) != usize::MAX {
                        return addr;
                    }
                    let h_mod = GetModuleHandleA(b"opengl32.dll\0".as_ptr() as *const _);
                    GetProcAddress(h_mod, c_name.as_ptr())
                },
                ctx: std::ptr::null_mut::<std::os::raw::c_void>(),
            };
            let params = vec![
                RenderParam::ApiType(RenderParamApiType::OpenGl),
                RenderParam::InitParams(init_params),
            ];
            let mpv_ref = unsafe {
                std::mem::transmute::<&Mpv, &'static Mpv>(&*self.mpv.lock().unwrap())
            };
            if let Ok(mut rc) = mpv_ref.create_render_context(params) {
                // Register mpv's frame-update callback → send wakeup to our repaint thread
                let tx = self.repaint_tx.clone();
                rc.set_update_callback(move || { let _ = tx.try_send(()); });
                TLS_RENDER_CTX.with(|c| *c.borrow_mut() = Some(rc));
                self.render_ctx_initialized = true;
            }
        }

        // ── Controls visibility logic ─────────────────────────────────────────
        let screen_h = ctx.screen_rect().height();
        let (mouse_moved, mouse_y) = ctx.input(|i| (
            i.pointer.delta().length_sq() > 0.5,
            i.pointer.hover_pos().map(|p| p.y).unwrap_or(0.0),
        ));

        if mouse_moved {
            self.last_mouse_move = std::time::Instant::now();
            self.show_controls = true;
        }

        if self.is_fullscreen && self.has_file {
            let idle_secs = self.last_mouse_move.elapsed().as_secs_f32();
            // Show if mouse is in bottom trigger zone OR recently moved
            let in_trigger = screen_h - mouse_y < TRIGGER_ZONE;
            self.show_controls = idle_secs < 2.5 || in_trigger;
        } else {
            // Windowed mode: always show controls
            self.show_controls = true;
        }

        // ── Input ─────────────────────────────────────────────────────────────
        ctx.input(|i| {
            // Drag and drop
            for file in &i.raw.dropped_files {
                if let Some(path) = &file.path {
                    let s = path.to_string_lossy().into_owned();
                    if is_video_file(&s) { self.open_file(&s); }
                }
            }
            // Keys
            if i.key_pressed(egui::Key::Space)      { self.toggle_play_pause(); }
            if i.key_pressed(egui::Key::ArrowRight) { self.skip(10.0); }
            if i.key_pressed(egui::Key::ArrowLeft)  { self.skip(-10.0); }
            if i.key_pressed(egui::Key::ArrowUp)    { self.set_volume(self.volume + 5.0); }
            if i.key_pressed(egui::Key::ArrowDown)  { self.set_volume(self.volume - 5.0); }
            if i.key_pressed(egui::Key::F) {
                self.is_fullscreen = !self.is_fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
            }
            if i.key_pressed(egui::Key::Escape) && self.is_fullscreen {
                self.is_fullscreen = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
        });

        // ── Video panel ───────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                let rect = ui.max_rect();

                // Double-click → toggle fullscreen
                if ui.allocate_rect(rect, egui::Sense::click()).double_clicked() {
                    self.is_fullscreen = !self.is_fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                }

                if self.render_ctx_initialized {
                    let (w, h) = (rect.width() as i32, rect.height() as i32);
                    ui.painter().add(egui::PaintCallback {
                        rect,
                        callback: Arc::new(egui_glow::CallbackFn::new(move |_info, _painter| {
                            TLS_RENDER_CTX.with(|c| {
                                if let Some(rc) = c.borrow_mut().as_mut() {
                                    let _ = rc.render::<*mut std::os::raw::c_void>(0, w, h, true);
                                }
                            });
                        })),
                    });
                } else if !self.has_file {
                    draw_drop_hint(ui, rect);
                }
            });

        // ── Control bar (overlay in fullscreen, panel in windowed) ────────────
        if self.show_controls {
            draw_controls(self, ctx);
        }

        // Schedule a time-display refresh every ~250ms while playing.
        // mpv's update callback handles actual frame repaints — we only need
        // this slow tick for the seek-bar clock to stay current.
        if self.is_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

// ── Drop hint ─────────────────────────────────────────────────────────────────
fn draw_drop_hint(ui: &mut egui::Ui, rect: egui::Rect) {
    let c = rect.center();
    let p = ui.painter();
    p.circle_stroke(c, 44.0, egui::Stroke::new(2.0, egui::Color32::from_rgb(70, 70, 82)));
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::Pos2::new(c.x - 11.0, c.y - 17.0),
            egui::Pos2::new(c.x - 11.0, c.y + 17.0),
            egui::Pos2::new(c.x + 22.0, c.y),
        ],
        egui::Color32::from_rgb(70, 70, 82),
        egui::Stroke::NONE,
    ));
    p.text(egui::Pos2::new(c.x, c.y + 68.0), egui::Align2::CENTER_CENTER,
        "Drop a video file to play", egui::FontId::proportional(17.0),
        egui::Color32::from_rgb(140, 140, 155));
    p.text(egui::Pos2::new(c.x, c.y + 92.0), egui::Align2::CENTER_CENTER,
        "MP4 · MKV · AVI · MOV · WMV · WebM · and more",
        egui::FontId::proportional(12.0), egui::Color32::from_rgb(90, 90, 105));
}

// ── Control bar ───────────────────────────────────────────────────────────────
fn draw_controls(app: &mut FluentMediaPlayer, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("controls")
        .exact_height(BAR_H)
        .frame(egui::Frame::none()
            .fill(BAR_BG)
            .inner_margin(egui::Margin { left: 10.0, right: 10.0, top: 6.0, bottom: 6.0 }))
        .show(ctx, |ui| {
            let total_w = ui.available_width();

            // ── Single row: [time] [seekbar] [time] [skip] [play] [skip] [vol_icon] [vol] [fs] ──
            ui.horizontal(|ui| {
                ui.set_height(BAR_H - 12.0); // respect vertical margin

                // Elapsed
                ui.label(egui::RichText::new(fmt_time(app.time_pos)).color(TEXT_DIM).size(11.0));
                ui.add_space(4.0);

                // Seek bar — takes up middle portion
                let seek_w = (total_w - 330.0).max(80.0);
                let (seek_rect, seek_resp) = ui.allocate_exact_size(
                    egui::Vec2::new(seek_w, BAR_H - 12.0),
                    egui::Sense::click_and_drag(),
                );
                let frac = if app.duration > 0.0 {
                    (app.time_pos / app.duration).clamp(0.0, 1.0) as f32
                } else { 0.0 };

                if seek_resp.is_pointer_button_down_on() || seek_resp.dragged() {
                    if let Some(pos) = seek_resp.interact_pointer_pos() {
                        let f = ((pos.x - seek_rect.left()) / seek_rect.width()).clamp(0.0, 1.0);
                        app.seek_to(f as f64 * app.duration);
                    }
                }

                // Draw seek track
                let p = ui.painter();
                let mid_y = seek_rect.center().y;
                let track_h = 3.0;
                let track = egui::Rect::from_min_max(
                    egui::Pos2::new(seek_rect.left(), mid_y - track_h / 2.0),
                    egui::Pos2::new(seek_rect.right(), mid_y + track_h / 2.0),
                );
                p.rect_filled(track, egui::Rounding::same(2.0), TRACK_BG);
                if frac > 0.0 {
                    p.rect_filled(
                        egui::Rect::from_min_max(track.min, egui::Pos2::new(track.left() + track.width() * frac, track.bottom())),
                        egui::Rounding::same(2.0), ACCENT,
                    );
                }
                let knob_x = seek_rect.left() + seek_rect.width() * frac;
                let knob_r = if seek_resp.hovered() || seek_resp.dragged() { 6.0 } else { 4.5 };
                p.circle_filled(egui::Pos2::new(knob_x, mid_y), knob_r, ACCENT);

                ui.add_space(4.0);
                // Duration
                ui.label(egui::RichText::new(fmt_time(app.duration)).color(TEXT_DIM).size(11.0));
                ui.add_space(8.0);

                // Skip back
                if icon_btn(ui, "⏮", 18.0) { app.skip(-10.0); }
                ui.add_space(4.0);

                // Play/pause — compact circle button
                let play_resp = ui.add_sized(
                    [30.0, 30.0],
                    egui::Button::new(
                        egui::RichText::new(if app.is_playing { "⏸" } else { "▶" })
                            .size(14.0).color(egui::Color32::BLACK)
                    ).fill(egui::Color32::WHITE).rounding(15.0),
                );
                if play_resp.clicked() { app.toggle_play_pause(); }
                ui.add_space(4.0);

                // Skip forward
                if icon_btn(ui, "⏭", 18.0) { app.skip(10.0); }
                ui.add_space(8.0);

                // Right-side controls (volume + fullscreen) — right-to-left
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_btn(ui, "⛶", 18.0) {
                        app.is_fullscreen = !app.is_fullscreen;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.is_fullscreen));
                    }
                    ui.add_space(4.0);
                    let vol_icon = if app.volume == 0.0 { "🔇" }
                        else if app.volume < 40.0 { "🔉" } else { "🔊" };
                    ui.label(egui::RichText::new(vol_icon).size(13.0).color(ICON_DIM));
                    let mut vol = app.volume as f32;
                    ui.spacing_mut().slider_width = 70.0;
                    if ui.add(egui::Slider::new(&mut vol, 0.0f32..=100.0)
                        .show_value(false).trailing_fill(true)).changed() {
                        app.set_volume(vol as f64);
                    }
                });
            });
        });
}

fn icon_btn(ui: &mut egui::Ui, icon: &str, size: f32) -> bool {
    ui.add(egui::Button::new(
        egui::RichText::new(icon).size(size).color(ICON_DIM)
    ).frame(false)).clicked()
}

fn fmt_time(secs: f64) -> String {
    let s = secs as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn is_video_file(path: &str) -> bool {
    matches!(
        std::path::Path::new(path).extension()
            .and_then(|e| e.to_str()).map(|s| s.to_lowercase()).as_deref(),
        Some("mp4"|"mkv"|"avi"|"mov"|"wmv"|"flv"|"webm"|"m4v"|"ts"|"m2ts"|"mpg"|"mpeg"|"3gp"|"ogv")
    )
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Media Player")
            .with_inner_size([960.0, 540.0])
            .with_min_inner_size([400.0, 300.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Media Player",
        options,
        Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))),
    )
}
