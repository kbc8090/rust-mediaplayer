#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams, RenderParam, RenderParamApiType}};
use std::sync::{Arc, Mutex};
use std::cell::RefCell;

// Windows OpenGL proc address lookup
unsafe extern "system" {
    fn wglGetProcAddress(lpszProc: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(lpModuleName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}

thread_local! {
    static TLS_RENDER_CTX: RefCell<Option<RenderContext<'static>>> = const { RefCell::new(None) };
}

// ── Palette ───────────────────────────────────────────────────────────────────
const ACCENT:      egui::Color32 = egui::Color32::from_rgb(255, 165, 0);
const BAR_BG:      egui::Color32 = egui::Color32::from_rgba_premultiplied(18, 18, 22, 230);
const TRACK_BG:    egui::Color32 = egui::Color32::from_rgb(55, 55, 65);
const ICON_DIM:    egui::Color32 = egui::Color32::from_rgb(200, 200, 210);
const TEXT_DIM:    egui::Color32 = egui::Color32::from_rgb(150, 150, 160);

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
    last_mouse_move: f64,
    // Drop-hint state
    has_file: bool,
}

impl FluentMediaPlayer {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mpv = Mpv::new().expect("Failed to initialize libmpv!");
        let _ = mpv.set_property("hwdec", "auto");
        let _ = mpv.set_property("osc", "no");
        let _ = mpv.set_property("keep-open", "yes"); // don't close on EOF
        let _ = mpv.set_property("volume", 80_i64);
        cc.egui_ctx.request_repaint();

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
            last_mouse_move: 0.0,
            has_file: false,
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
        // ── Poll mpv properties ───────────────────────────────────────────────
        if let Ok(mpv) = self.mpv.lock() {
            if let Ok(t) = mpv.get_property::<f64>("time-pos")   { self.time_pos = t; }
            if let Ok(d) = mpv.get_property::<f64>("duration")   { self.duration = d; }
            if let Ok(v) = mpv.get_property::<f64>("volume")     { self.volume   = v; }
            // Sync play state from mpv (handles natural EOF etc)
            if let Ok(p) = mpv.get_property::<bool>("pause")     { self.is_playing = !p; }
        }

        // ── Initialize mpv OpenGL render context once GL is available ─────────
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
            // SAFETY: render context lifetime is tied to mpv which lives as long as the app
            let mpv_ref = unsafe {
                std::mem::transmute::<&Mpv, &'static Mpv>(&*self.mpv.lock().unwrap())
            };
            if let Ok(rc) = mpv_ref.create_render_context(params) {
                TLS_RENDER_CTX.with(|c| *c.borrow_mut() = Some(rc));
                self.render_ctx_initialized = true;
            }
        }

        // ── Input handling ────────────────────────────────────────────────────
        ctx.input(|i| {
            // Drag and drop
            for file in &i.raw.dropped_files {
                if let Some(path) = &file.path {
                    let path_str = path.to_string_lossy().into_owned();
                    if is_video_file(&path_str) {
                        self.open_file(&path_str);
                    }
                }
            }

            // Mouse move → show controls
            if i.pointer.delta().length_sq() > 0.0 {
                self.last_mouse_move = i.time;
                self.show_controls = true;
            } else if self.is_fullscreen && self.has_file && i.time - self.last_mouse_move > 2.5 {
                self.show_controls = false;
            }

            // Keyboard shortcuts
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

        // ── Video panel (solid black — NO transparency) ───────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                let rect = ui.max_rect();

                // Double-click → fullscreen
                if ui.allocate_rect(rect, egui::Sense::click()).double_clicked() {
                    self.is_fullscreen = !self.is_fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                }

                if self.render_ctx_initialized {
                    // mpv renders directly into this OpenGL rect
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
                    // Drop hint while no file loaded
                    draw_drop_hint(ui, rect);
                }
            });

        // ── Control bar overlay ───────────────────────────────────────────────
        if self.show_controls {
            draw_controls(self, ctx);
        }

        // Keep repainting while playing so time_pos updates
        if self.is_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
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
        .frame(egui::Frame::none().fill(BAR_BG).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
        .show(ctx, |ui| {
            let total_w = ui.available_width();

            // ── Seek bar row ──────────────────────────────────────────────────
            ui.horizontal(|ui| {
                // Elapsed time
                ui.label(egui::RichText::new(fmt_time(app.time_pos))
                    .color(TEXT_DIM).size(11.0));

                // Seek bar — custom drawn
                let bar_w = total_w - 90.0;
                let (seek_rect, seek_resp) = ui.allocate_exact_size(
                    egui::Vec2::new(bar_w, 16.0),
                    egui::Sense::click_and_drag(),
                );
                let frac = if app.duration > 0.0 {
                    (app.time_pos / app.duration).clamp(0.0, 1.0) as f32
                } else { 0.0 };

                // Handle seek interaction
                if seek_resp.is_pointer_button_down_on() || seek_resp.dragged() {
                    if let Some(p) = seek_resp.interact_pointer_pos() {
                        let f = ((p.x - seek_rect.left()) / seek_rect.width()).clamp(0.0, 1.0);
                        app.seek_to(f as f64 * app.duration);
                    }
                }

                let p = ui.painter();
                let mid_y = seek_rect.center().y;
                let track = egui::Rect::from_min_max(
                    egui::Pos2::new(seek_rect.left(), mid_y - 2.0),
                    egui::Pos2::new(seek_rect.right(), mid_y + 2.0),
                );
                p.rect_filled(track, egui::Rounding::same(2.0), TRACK_BG);
                if frac > 0.0 {
                    let filled = egui::Rect::from_min_max(
                        track.min,
                        egui::Pos2::new(track.left() + track.width() * frac, track.bottom()),
                    );
                    p.rect_filled(filled, egui::Rounding::same(2.0), ACCENT);
                }
                // Knob
                let knob_x = seek_rect.left() + seek_rect.width() * frac;
                let hovered = seek_resp.hovered() || seek_resp.dragged();
                let knob_r = if hovered { 7.0 } else { 5.5 };
                p.circle_filled(egui::Pos2::new(knob_x, mid_y), knob_r, ACCENT);

                // Duration
                ui.label(egui::RichText::new(fmt_time(app.duration))
                    .color(TEXT_DIM).size(11.0));
            });

            ui.add_space(4.0);

            // ── Transport row ─────────────────────────────────────────────────
            ui.horizontal(|ui| {
                // File name (left side)
                let name = app.file_stem().to_owned();
                if !name.is_empty() {
                    let truncated = if name.chars().count() > 35 {
                        format!("{}…", name.chars().take(34).collect::<String>())
                    } else { name };
                    ui.label(egui::RichText::new(truncated)
                        .color(egui::Color32::from_rgb(210, 210, 220)).size(12.0));
                }

                // Push buttons to center
                let side_w = (total_w - 220.0) / 2.0;
                ui.add_space((side_w - ui.min_rect().width()).max(0.0));

                // Skip back 10s
                if icon_btn(ui, "⏮", 26.0) { app.skip(-10.0); }
                ui.add_space(4.0);

                // Play/pause (big white circle)
                let play_resp = ui.add_sized(
                    [40.0, 40.0],
                    egui::Button::new(
                        egui::RichText::new(if app.is_playing { "⏸" } else { "▶" })
                            .size(18.0).color(egui::Color32::BLACK)
                    ).fill(egui::Color32::WHITE).rounding(20.0),
                );
                if play_resp.clicked() { app.toggle_play_pause(); }
                ui.add_space(4.0);

                // Skip forward 10s
                if icon_btn(ui, "⏭", 26.0) { app.skip(10.0); }

                // Push volume to right
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Fullscreen button
                    if icon_btn(ui, "⛶", 26.0) {
                        app.is_fullscreen = !app.is_fullscreen;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.is_fullscreen));
                    }
                    ui.add_space(8.0);

                    // Volume slider (80px wide)
                    let vol_icon = if app.volume == 0.0 { "🔇" }
                        else if app.volume < 40.0 { "🔉" }
                        else { "🔊" };
                    ui.label(egui::RichText::new(vol_icon).size(14.0).color(ICON_DIM));

                    let mut vol = app.volume as f32;
                    let vol_resp = ui.add(
                        egui::Slider::new(&mut vol, 0.0f32..=100.0)
                            .show_value(false)
                            .trailing_fill(true)
                    );
                    if vol_resp.changed() { app.set_volume(vol as f64); }
                });
            });
        });
}

fn icon_btn(ui: &mut egui::Ui, icon: &str, size: f32) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(icon).size(size).color(ICON_DIM))
            .frame(false)
    ).clicked()
}

fn fmt_time(secs: f64) -> String {
    let s = secs as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

fn is_video_file(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());
    matches!(ext.as_deref(), Some(
        "mp4"|"mkv"|"avi"|"mov"|"wmv"|"flv"|"webm"|"m4v"|"ts"|"m2ts"|"mpg"|"mpeg"|"3gp"|"ogv"
    ))
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Media Player")
            .with_inner_size([960.0, 540.0])
            .with_min_inner_size([400.0, 300.0])
            .with_drag_and_drop(true),
            // NOTE: with_transparent removed — that was causing the invisible window
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Media Player",
        options,
        Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))),
    )
}
