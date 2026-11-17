#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, Pos2, Rect, Sense, Stroke};
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams, RenderParam, RenderParamApiType}};
use std::sync::{Arc, Mutex};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

unsafe extern "system" {
    fn wglGetProcAddress(lpszProc: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(lpModuleName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}

thread_local! {
    static TLS_RENDER_CTX: RefCell<Option<RenderContext<'static>>> = const { RefCell::new(None) };
}

// ── Palette (sampled from the reference screenshot) ───────────────────────────
const ACCENT:     Color32 = Color32::from_rgb(254, 188, 127); // seek / volume fill
const KNOB_RING:  Color32 = Color32::from_rgb(70, 70, 70);    // dark ring around the knob
const TRACK:      Color32 = Color32::from_rgb(190, 184, 180); // unplayed seek track
const VOL_TRACK:  Color32 = Color32::from_rgb(160, 158, 155); // unplayed volume track
const WIN_BORDER: Color32 = Color32::from_rgb(88, 66, 44);

// ── Layout (points) ───────────────────────────────────────────────────────────
const TOP_H: f32 = 64.0;              // title strip + gradient height
const BOTTOM_H: f32 = 120.0;          // control area + gradient height
const SIDE: f32 = 20.0;               // left/right margin of the seek bar
const SEEK_FROM_BOTTOM: f32 = 69.0;   // seek bar centre line
const ROW_FROM_BOTTOM: f32 = 33.0;    // button row centre line
const WIN_BTN_W: f32 = 46.0;
const WIN_BTN_H: f32 = 34.0;
const MORE_POPUP: &str = "more_popup";

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    render_ctx_initialized: bool,
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
    time_pos: f64,
    duration: f64,
    volume: f64,
    muted: bool,
    repeat: bool,
    /// Visual toggle only for now (there is no playlist yet)
    shuffle: bool,
    on_top: bool,
    speed: f64,
    show_controls: bool,
    /// Fraction of the seek bar while the user is dragging it
    drag_frac: Option<f32>,
    /// Wall-clock instant of last user activity, used for auto-hide
    last_mouse_move: std::time::Instant,
    has_file: bool,
    /// Wakeup sender so mpv can ask for repaints from its own thread
    repaint_tx: std::sync::mpsc::SyncSender<()>,
    /// Video files in the current file's folder, in Explorer-style order
    folder_files: Vec<PathBuf>,
    folder_index: Option<usize>,
    /// Accumulated mouse-wheel scroll (points) not yet converted into volume steps
    scroll_accum: f32,
}

impl FluentMediaPlayer {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);

        // video-timing-offset = 0 stops mpv from blocking the UI thread inside
        // render() while it waits for each frame's display time (the default 50 ms
        // wait made window moves/resizes follow the video's frame rate).
        let mpv = Mpv::with_initializer(|init| {
            init.set_property("video-timing-offset", 0.0_f64)?;
            Ok(())
        })
        .expect("Failed to initialize libmpv!");
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
            muted: false,
            repeat: false,
            shuffle: false,
            on_top: false,
            speed: 1.0,
            show_controls: true,
            drag_frac: None,
            last_mouse_move: std::time::Instant::now(),
            has_file: false,
            repaint_tx: tx,
            folder_files: Vec::new(),
            folder_index: None,
            scroll_accum: 0.0,
        }
    }

    fn open_file(&mut self, path: &str) {
        self.current_file = path.to_owned();
        self.has_file = true;
        self.folder_files = scan_folder(Path::new(path));
        self.folder_index = self.folder_files.iter().position(|p| p == Path::new(path));
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

    fn set_mute(&mut self, mute: bool) {
        self.muted = mute;
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.set_property("mute", mute);
        }
    }

    fn toggle_repeat(&mut self) {
        self.repeat = !self.repeat;
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.set_property("loop-file", if self.repeat { "inf" } else { "no" });
        }
    }

    fn cycle_captions(&mut self) {
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.command("cycle", &["sub"]);
        }
    }

    fn set_speed(&mut self, speed: f64) {
        self.speed = speed;
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.set_property("speed", speed);
        }
    }

    fn has_prev(&self) -> bool {
        self.folder_index.map_or(false, |i| i > 0)
    }

    fn has_next(&self) -> bool {
        self.folder_index.map_or(false, |i| i + 1 < self.folder_files.len())
    }

    /// Open the previous (-1) or next (+1) video in the current file's folder
    fn play_adjacent(&mut self, delta: isize) {
        let Some(i) = self.folder_index else { return };
        let target = i as isize + delta;
        if target < 0 || target as usize >= self.folder_files.len() { return; }
        let path = self.folder_files[target as usize].to_string_lossy().into_owned();
        self.open_file(&path);
    }

    fn skip(&mut self, secs: f64) {
        let new_pos = (self.time_pos + secs).clamp(0.0, self.duration);
        self.seek_to(new_pos);
    }

    fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.is_fullscreen = !self.is_fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
    }

    fn toggle_maximize(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    }

    fn toggle_on_top(&mut self, ctx: &egui::Context) {
        self.on_top = !self.on_top;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if self.on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
    }

    fn file_name(&self) -> String {
        std::path::Path::new(&self.current_file)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned()
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
            if let Ok(m) = mpv.get_property::<bool>("mute")    { self.muted = m; }
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

        // ── Read all input first (no viewport commands inside the closure) ────
        let screen = ctx.screen_rect();
        let inp = ctx.input(|i| InputSnapshot {
            dropped: i.raw.dropped_files.iter()
                .filter_map(|f| f.path.as_ref().map(|p| p.to_string_lossy().into_owned()))
                .collect(),
            space: i.key_pressed(egui::Key::Space),
            right: i.key_pressed(egui::Key::ArrowRight),
            left:  i.key_pressed(egui::Key::ArrowLeft),
            up:    i.key_pressed(egui::Key::ArrowUp),
            down:  i.key_pressed(egui::Key::ArrowDown),
            f:     i.key_pressed(egui::Key::F),
            m:     i.key_pressed(egui::Key::M),
            n:     i.key_pressed(egui::Key::N),
            p:     i.key_pressed(egui::Key::P),
            esc:   i.key_pressed(egui::Key::Escape),
            scroll_y: i.raw_scroll_delta.y,
            pressed: i.pointer.primary_pressed(),
            moved: i.pointer.delta().length_sq() > 0.5,
            active: i.pointer.any_down()
                || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })),
            pointer: i.pointer.hover_pos(),
        });

        // ── Controls visibility logic ─────────────────────────────────────────
        if inp.moved || inp.active || inp.scroll_y != 0.0 {
            self.last_mouse_move = std::time::Instant::now();
        }
        let near_edge = inp.pointer
            .map_or(false, |p| p.y < TOP_H || p.y > screen.bottom() - BOTTOM_H);
        let menu_open = ctx.memory(|m| m.is_popup_open(egui::Id::new(MORE_POPUP)));
        self.show_controls = !self.has_file
            || !self.is_playing
            || menu_open
            || near_edge
            || self.last_mouse_move.elapsed().as_secs_f32() < 2.5;
        if !self.show_controls {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        // ── Actions ───────────────────────────────────────────────────────────
        for s in &inp.dropped {
            if is_video_file(s) { self.open_file(s); }
        }
        if inp.space { self.toggle_play_pause(); }
        if inp.right { self.skip(10.0); }
        if inp.left  { self.skip(-10.0); }
        if inp.up    { self.set_volume(self.volume + 5.0); }
        if inp.down  { self.set_volume(self.volume - 5.0); }
        if inp.m     { let m = !self.muted; self.set_mute(m); }
        if inp.n     { self.play_adjacent(1); }
        if inp.p     { self.play_adjacent(-1); }

        // Mouse wheel anywhere in the window = volume, 5 % per notch
        // (egui reports one wheel notch as 50 points)
        self.scroll_accum += inp.scroll_y;
        let steps = (self.scroll_accum / 50.0).trunc();
        if steps != 0.0 {
            self.scroll_accum -= steps * 50.0;
            self.set_volume(self.volume + steps as f64 * 5.0);
            if steps > 0.0 && self.muted { self.set_mute(false); }
        }

        if inp.f     { self.toggle_fullscreen(ctx); }
        if inp.esc && self.is_fullscreen {
            self.is_fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }

        // ── Resize from the window edges (we have no OS frame) ────────────────
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let edge = if self.is_fullscreen || maximized {
            None
        } else {
            inp.pointer.and_then(|p| hit_edge(p, screen))
        };
        if let Some((dir, _)) = edge {
            if inp.pressed {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
        }

        // ── Video panel: full-bleed, the controls float on top of it ──────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::BLACK))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let resp = ui.allocate_rect(rect, Sense::click_and_drag());

                // Double-click → toggle fullscreen
                if resp.double_clicked() {
                    self.toggle_fullscreen(ctx);
                }
                // Drag anywhere on the video → move the window
                if resp.drag_started() && !self.is_fullscreen && edge.is_none() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                if self.render_ctx_initialized {
                    ui.painter().add(egui::PaintCallback {
                        rect,
                        callback: Arc::new(egui_glow::CallbackFn::new(move |info, _painter| {
                            // Render at the real pixel size so HiDPI scaling stays correct
                            let vp = info.viewport_in_pixels();
                            TLS_RENDER_CTX.with(|c| {
                                if let Some(rc) = c.borrow_mut().as_mut() {
                                    let _ = rc.render::<*mut std::os::raw::c_void>(
                                        0, vp.width_px, vp.height_px, true,
                                    );
                                }
                            });
                        })),
                    });
                } else if !self.has_file {
                    draw_drop_hint(ui, rect);
                }
            });

        // ── Overlay: title bar + controls (fade in/out) ───────────────────────
        let t = ctx.animate_bool_with_time(egui::Id::new("overlay_vis"), self.show_controls, 0.2);
        if t > 0.0 {
            draw_top_bar(self, ctx, t);
            draw_bottom_bar(self, ctx, t);
        }

        // Resize cursor wins over any other cursor set this frame
        if let Some((_, icon)) = edge {
            ctx.set_cursor_icon(icon);
        }

        // Thin window border (we have no OS frame any more)
        if !self.is_fullscreen {
            if !maximized {
                ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("border")))
                    .rect_stroke(screen.shrink(0.5), 0.0, Stroke::new(1.0_f32, WIN_BORDER));
            }
        }

        // Schedule a time-display refresh every ~250ms while playing.
        // mpv's update callback handles actual frame repaints — we only need
        // this slow tick for the seek-bar clock and the auto-hide timer.
        if self.is_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Quit immediately. Letting Rust tear down mpv, the GL context and the audio
    /// device the normal way takes a noticeable moment (busy cursor); there is
    /// nothing to save, so just end the process.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        std::process::exit(0);
    }
}

struct InputSnapshot {
    dropped: Vec<String>,
    space: bool, right: bool, left: bool, up: bool, down: bool,
    f: bool, m: bool, n: bool, p: bool, esc: bool,
    scroll_y: f32,
    pressed: bool,
    moved: bool, active: bool,
    pointer: Option<Pos2>,
}

// ── Fonts: use Segoe UI if Windows has it, otherwise egui's defaults ──────────
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), egui::FontData::from_owned(bytes));
        if let Some(fam) = fonts.families.get_mut(&FontFamily::Proportional) {
            fam.insert(0, "segoe".into());
        }
    }

    // "semibold" family always exists (falls back to the proportional fonts)
    let mut semibold = fonts.families
        .get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\seguisb.ttf") {
        fonts.font_data.insert("segoe_sb".into(), egui::FontData::from_owned(bytes));
        semibold.insert(0, "segoe_sb".into());
    }
    fonts.families.insert(FontFamily::Name("semibold".into()), semibold);

    ctx.set_fonts(fonts);
}

// ── Drop hint ─────────────────────────────────────────────────────────────────
fn draw_drop_hint(ui: &mut egui::Ui, rect: Rect) {
    let c = rect.center();
    let p = ui.painter();
    p.circle_stroke(c, 44.0, Stroke::new(2.0_f32, Color32::from_rgb(70, 70, 82)));
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - 11.0, c.y - 17.0),
            pos2(c.x - 11.0, c.y + 17.0),
            pos2(c.x + 22.0, c.y),
        ],
        Color32::from_rgb(70, 70, 82),
        Stroke::NONE,
    ));
    p.text(pos2(c.x, c.y + 68.0), Align2::CENTER_CENTER,
        "Drop a video file to play", FontId::proportional(17.0),
        Color32::from_rgb(140, 140, 155));
    p.text(pos2(c.x, c.y + 92.0), Align2::CENTER_CENTER,
        "MP4 · MKV · AVI · MOV · WMV · WebM · and more",
        FontId::proportional(12.0), Color32::from_rgb(90, 90, 105));
}

// ── Small drawing helpers ─────────────────────────────────────────────────────
fn black(alpha: u8, t: f32) -> Color32 {
    Color32::from_black_alpha((alpha as f32 * t) as u8)
}

/// Vertical two-colour gradient rectangle
fn gradient(p: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(rect.left_top(), top);
    m.colored_vertex(rect.right_top(), top);
    m.colored_vertex(rect.right_bottom(), bottom);
    m.colored_vertex(rect.left_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(m));
}

/// Polyline given as offsets from a centre point
fn poly(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x, c.y + y)).collect();
    p.add(egui::Shape::line(v, Stroke::new(w, col)));
}

fn closed_poly(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x, c.y + y)).collect();
    p.add(egui::Shape::closed_line(v, Stroke::new(w, col)));
}

/// 36×36 hit area with a soft round hover highlight. Draw the icon afterwards.
fn hit(ui: &mut egui::Ui, centre: Pos2, id: &str, tip: &str, t: f32) -> egui::Response {
    let r = ui.interact(
        Rect::from_center_size(centre, vec2(36.0, 36.0)),
        ui.id().with(id),
        Sense::click(),
    );
    if r.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().circle_filled(centre, 17.0, Color32::from_white_alpha((24.0 * t) as u8));
    }
    r.on_hover_text(tip)
}

// ── Icons (drawn with shapes so they never depend on font coverage) ───────────
const SW: f32 = 1.4;

fn icon_pause(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_filled(Rect::from_center_size(pos2(c.x - 3.5, c.y), vec2(2.6, 13.0)), 0.5, col);
    p.rect_filled(Rect::from_center_size(pos2(c.x + 3.5, c.y), vec2(2.6, 13.0)), 0.5, col);
}
fn icon_play(p: &egui::Painter, c: Pos2, col: Color32) {
    p.add(egui::Shape::convex_polygon(
        vec![pos2(c.x - 4.5, c.y - 7.5), pos2(c.x - 4.5, c.y + 7.5), pos2(c.x + 7.5, c.y)],
        col, Stroke::NONE,
    ));
}
fn icon_prev(p: &egui::Painter, c: Pos2, col: Color32) {
    p.line_segment([pos2(c.x - 6.5, c.y - 6.0), pos2(c.x - 6.5, c.y + 6.0)], Stroke::new(SW, col));
    closed_poly(p, c, &[(6.5, -6.0), (6.5, 6.0), (-3.0, 0.0)], col, SW);
}
fn icon_next(p: &egui::Painter, c: Pos2, col: Color32) {
    p.line_segment([pos2(c.x + 6.5, c.y - 6.0), pos2(c.x + 6.5, c.y + 6.0)], Stroke::new(SW, col));
    closed_poly(p, c, &[(-6.5, -6.0), (-6.5, 6.0), (3.0, 0.0)], col, SW);
}
fn icon_volume(p: &egui::Painter, c: Pos2, col: Color32, level: f64, muted: bool) {
    let c = pos2(c.x - 3.0, c.y);
    closed_poly(p, c, &[(-6.0, -3.0), (-3.0, -3.0), (1.5, -7.0), (1.5, 7.0), (-3.0, 3.0), (-6.0, 3.0)], col, SW);
    if muted || level <= 0.0 {
        poly(p, c, &[(5.0, -3.5), (10.5, 3.5)], col, SW);
        poly(p, c, &[(5.0, 3.5), (10.5, -3.5)], col, SW);
    } else {
        let arc = |r: f32| -> Vec<(f32, f32)> {
            (-4..=4).map(|k| {
                let a = (k as f32) * 11.0_f32.to_radians();
                (1.5 + r * a.cos(), r * a.sin())
            }).collect()
        };
        poly(p, c, &arc(5.0), col, SW);
        if level >= 40.0 { poly(p, c, &arc(9.0), col, SW); }
    }
}
fn icon_shuffle(p: &egui::Painter, c: Pos2, col: Color32) {
    poly(p, c, &[(-8.0, -4.0), (-3.0, -4.0), (3.0, 4.0), (7.0, 4.0)], col, SW);
    poly(p, c, &[(-8.0, 4.0), (-3.0, 4.0), (3.0, -4.0), (7.0, -4.0)], col, SW);
    poly(p, c, &[(4.5, 1.5), (7.5, 4.0), (4.5, 6.5)], col, SW);
    poly(p, c, &[(4.5, -6.5), (7.5, -4.0), (4.5, -1.5)], col, SW);
}
fn icon_repeat(p: &egui::Painter, c: Pos2, col: Color32) {
    poly(p, c, &[(-8.0, 2.0), (-8.0, -2.0), (-5.0, -5.0), (6.0, -5.0)], col, SW);
    poly(p, c, &[(3.5, -7.5), (6.0, -5.0), (3.5, -2.5)], col, SW);
    poly(p, c, &[(8.0, -2.0), (8.0, 2.0), (5.0, 5.0), (-6.0, 5.0)], col, SW);
    poly(p, c, &[(-3.5, 2.5), (-6.0, 5.0), (-3.5, 7.5)], col, SW);
}
fn icon_captions(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_stroke(Rect::from_center_size(c, vec2(18.0, 13.0)), 2.0, Stroke::new(SW, col));
    poly(p, c, &[(-5.0, -1.0), (-1.5, -1.0)], col, SW);
    poly(p, c, &[(1.5, -1.0), (5.0, -1.0)], col, SW);
    poly(p, c, &[(-5.0, 2.5), (2.0, 2.5)], col, SW);
}
fn icon_compact(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_stroke(Rect::from_center_size(c, vec2(17.0, 13.0)), 1.5, Stroke::new(SW, col));
    p.rect_filled(Rect::from_min_size(pos2(c.x + 0.5, c.y - 0.5), vec2(6.0, 4.5)), 0.5, col);
}
fn icon_fullscreen(p: &egui::Painter, c: Pos2, col: Color32) {
    poly(p, c, &[(-6.5, 6.5), (6.5, -6.5)], col, SW);
    poly(p, c, &[(6.5, -1.5), (6.5, -6.5), (1.5, -6.5)], col, SW);
    poly(p, c, &[(-6.5, 1.5), (-6.5, 6.5), (-1.5, 6.5)], col, SW);
}
fn icon_more(p: &egui::Painter, c: Pos2, col: Color32) {
    for dx in [-6.0, 0.0, 6.0] {
        p.circle_filled(pos2(c.x + dx, c.y), 1.5, col);
    }
}

// ── Title bar (overlaid on the video) ─────────────────────────────────────────
fn draw_top_bar(app: &mut FluentMediaPlayer, ctx: &egui::Context, t: f32) {
    let screen = ctx.screen_rect();
    let bar = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_H));
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let fullscreen = app.is_fullscreen;
    let title = if app.has_file { app.file_name() } else { "Media Player".to_owned() };
    let white = Color32::WHITE.gamma_multiply(t);

    egui::Area::new(egui::Id::new("top_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(bar.min)
        .show(ctx, |ui| {
            ui.allocate_rect(bar, Sense::hover());
            let p = ui.painter().clone();
            gradient(&p, bar, black(150, t), Color32::TRANSPARENT);

            // Drag region: move the window, double-click to maximise
            if !fullscreen {
                let drag = ui.interact(
                    Rect::from_min_size(bar.min, vec2(bar.width(), 40.0)),
                    ui.id().with("drag"),
                    Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if drag.double_clicked() {
                    app.toggle_maximize(ctx);
                }
            }

            // Left: file name
            let y = 29.0;
            let right_limit = if fullscreen { screen.right() - 20.0 } else { screen.right() - WIN_BTN_W * 3.0 - 10.0 };
            let clip = p.with_clip_rect(Rect::from_min_max(pos2(20.0, 0.0), pos2(right_limit, TOP_H)));
            let font = FontId::new(17.0, FontFamily::Name("semibold".into()));
            clip.text(pos2(21.0, y + 1.0), Align2::LEFT_CENTER, &title, font.clone(), black(170, t));
            clip.text(pos2(20.0, y), Align2::LEFT_CENTER, &title, font, white);

            // Right: window buttons (not shown in fullscreen)
            if !fullscreen {
                for k in 0..3 {
                    let r = Rect::from_min_size(
                        pos2(screen.right() - WIN_BTN_W * (3 - k) as f32, screen.top()),
                        vec2(WIN_BTN_W, WIN_BTN_H),
                    );
                    let resp = ui.interact(r, ui.id().with(("winbtn", k)), Sense::click());
                    if resp.hovered() {
                        let fill = if k == 2 {
                            Color32::from_rgb(232, 17, 35).gamma_multiply(t)
                        } else {
                            Color32::from_white_alpha((28.0 * t) as u8)
                        };
                        p.rect_filled(r, 0.0, fill);
                    }
                    let c = r.center();
                    let s = Stroke::new(1.1_f32, white);
                    match k {
                        0 => { p.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], s); }
                        1 => {
                            if maximized {
                                p.rect_stroke(Rect::from_min_size(pos2(c.x - 5.0, c.y - 2.0), vec2(7.0, 7.0)), 0.0, s);
                                poly(&p, c, &[(-2.0, -2.0), (-2.0, -5.0), (5.0, -5.0), (5.0, 2.0), (2.0, 2.0)], white, 1.1);
                            } else {
                                p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), 0.0, s);
                            }
                        }
                        _ => {
                            p.line_segment([pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)], s);
                            p.line_segment([pos2(c.x - 5.0, c.y + 5.0), pos2(c.x + 5.0, c.y - 5.0)], s);
                        }
                    }
                    if resp.clicked() {
                        match k {
                            0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                            1 => app.toggle_maximize(ctx),
                            _ => {
                                // Hide first so the window vanishes instantly
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    }
                }
            }
        });
}

// ── Seek bar + control row (overlaid on the video) ────────────────────────────
fn draw_bottom_bar(app: &mut FluentMediaPlayer, ctx: &egui::Context, t: f32) {
    let screen = ctx.screen_rect();
    let bar = Rect::from_min_max(pos2(screen.left(), screen.bottom() - BOTTOM_H), screen.right_bottom());
    let white = Color32::WHITE.gamma_multiply(t);
    let accent = ACCENT.gamma_multiply(t);

    egui::Area::new(egui::Id::new("bottom_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(bar.min)
        .show(ctx, |ui| {
            ui.allocate_rect(bar, Sense::hover());
            let p = ui.painter().clone();
            gradient(&p, bar, Color32::TRANSPARENT, black(175, t));

            // ── Seek bar ──────────────────────────────────────────────────────
            let seek_y = screen.bottom() - SEEK_FROM_BOTTOM;
            let sr = Rect::from_min_max(
                pos2(screen.left() + SIDE, seek_y - 12.0),
                pos2(screen.right() - SIDE, seek_y + 12.0),
            );
            let seek = ui.interact(sr, ui.id().with("seek"), Sense::click_and_drag());
            let frac_at = |x: f32| ((x - sr.left()) / sr.width()).clamp(0.0, 1.0);
            let dragging = seek.is_pointer_button_down_on() || seek.dragged();
            if dragging {
                if let Some(pos) = seek.interact_pointer_pos() {
                    let f = frac_at(pos.x);
                    app.drag_frac = Some(f);
                    if app.duration > 0.0 { app.seek_to(f as f64 * app.duration); }
                }
            } else {
                app.drag_frac = None;
            }
            let frac = app.drag_frac.unwrap_or(if app.duration > 0.0 {
                (app.time_pos / app.duration).clamp(0.0, 1.0) as f32
            } else { 0.0 });
            let hot = seek.hovered() || dragging;

            let track_h = if hot { 3.0 } else { 2.0 };
            p.rect_filled(
                Rect::from_min_max(pos2(sr.left(), seek_y - track_h / 2.0), pos2(sr.right(), seek_y + track_h / 2.0)),
                1.0, TRACK.gamma_multiply(t),
            );
            let fill_x = sr.left() + sr.width() * frac;
            p.rect_filled(
                Rect::from_min_max(pos2(sr.left(), seek_y - 1.5), pos2(fill_x, seek_y + 1.5)),
                1.0, accent,
            );
            let (ro, ri) = if hot { (9.0, 5.0) } else { (8.0, 4.5) };
            p.circle_filled(pos2(fill_x, seek_y), ro, KNOB_RING.gamma_multiply(t));
            p.circle_filled(pos2(fill_x, seek_y), ri, accent);

            // Hover / drag time preview
            let preview = if dragging { app.drag_frac }
                else if seek.hovered() { seek.hover_pos().map(|pp| frac_at(pp.x)) }
                else { None };
            if let (Some(f), true) = (preview, app.duration > 0.0) {
                let txt = fmt_time(f as f64 * app.duration);
                let font = FontId::proportional(12.0);
                let w = p.layout_no_wrap(txt.clone(), font.clone(), Color32::WHITE).size().x + 16.0;
                let cx = (sr.left() + sr.width() * f).clamp(sr.left() + w / 2.0, sr.right() - w / 2.0);
                let r = Rect::from_center_size(pos2(cx, seek_y - 28.0), vec2(w, 22.0));
                p.rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(32, 32, 32, (235.0 * t) as u8));
                p.text(r.center(), Align2::CENTER_CENTER, txt, font, white);
            }

            // ── Button row ────────────────────────────────────────────────────
            let row_y = screen.bottom() - ROW_FROM_BOTTOM;
            let left = screen.left();
            let right = screen.right();

            // Play / pause
            let c = pos2(left + 27.0, row_y);
            if hit(ui, c, "play", if app.is_playing { "Pause (Space)" } else { "Play (Space)" }, t).clicked() {
                app.toggle_play_pause();
            }
            if app.is_playing { icon_pause(&p, c, white) } else { icon_play(&p, c, white) }

            // Previous / next file in the same folder (dimmed when there isn't one)
            let dim = white.gamma_multiply(0.4);
            let c = pos2(left + 71.0, row_y);
            if hit(ui, c, "prev", "Previous file (P)", t).clicked() { app.play_adjacent(-1); }
            icon_prev(&p, c, if app.has_prev() { white } else { dim });

            let c = pos2(left + 111.0, row_y);
            if hit(ui, c, "next", "Next file (N)", t).clicked() { app.play_adjacent(1); }
            icon_next(&p, c, if app.has_next() { white } else { dim });

            // Time
            p.text(
                pos2(left + 139.0, row_y), Align2::LEFT_CENTER,
                format!("{} / {}", fmt_time(app.time_pos), fmt_time(app.duration)),
                FontId::proportional(13.0), white,
            );

            // Volume icon (click = mute) + slider
            let c = pos2(right - 400.0, row_y);
            if hit(ui, c, "mute", if app.muted { "Unmute (M)" } else { "Mute (M)" }, t).clicked() {
                let m = !app.muted;
                app.set_mute(m);
            }
            icon_volume(&p, c, white, app.volume, app.muted);

            let vl = right - 370.0;
            let vw = 93.0;
            let vr = Rect::from_min_max(pos2(vl - 8.0, row_y - 12.0), pos2(vl + vw + 8.0, row_y + 12.0));
            let vresp = ui.interact(vr, ui.id().with("vol"), Sense::click_and_drag());
            if vresp.is_pointer_button_down_on() || vresp.dragged() {
                if let Some(pos) = vresp.interact_pointer_pos() {
                    let v = ((pos.x - vl) / vw).clamp(0.0, 1.0) as f64 * 100.0;
                    app.set_volume(v);
                    if app.muted { app.set_mute(false); }
                }
            }
            let vf = (app.volume / 100.0).clamp(0.0, 1.0) as f32;
            let vhot = vresp.hovered() || vresp.dragged();
            p.rect_filled(
                Rect::from_min_max(pos2(vl, row_y - 1.0), pos2(vl + vw, row_y + 1.0)),
                1.0, VOL_TRACK.gamma_multiply(t),
            );
            p.rect_filled(
                Rect::from_min_max(pos2(vl, row_y - 1.5), pos2(vl + vw * vf, row_y + 1.5)),
                1.0, accent,
            );
            let (ro, ri) = if vhot { (8.0, 4.5) } else { (7.0, 4.0) };
            p.circle_filled(pos2(vl + vw * vf, row_y), ro, KNOB_RING.gamma_multiply(t));
            p.circle_filled(pos2(vl + vw * vf, row_y), ri, accent);

            // Right-hand buttons
            let tint = |on: bool| if on { accent } else { white };

            let c = pos2(right - 248.0, row_y);
            if hit(ui, c, "shuffle", "Shuffle (needs a playlist — not wired up yet)", t).clicked() {
                app.shuffle = !app.shuffle;
            }
            icon_shuffle(&p, c, tint(app.shuffle));

            let c = pos2(right - 203.0, row_y);
            if hit(ui, c, "repeat", "Repeat this video", t).clicked() { app.toggle_repeat(); }
            icon_repeat(&p, c, tint(app.repeat));

            let c = pos2(right - 159.0, row_y);
            if hit(ui, c, "captions", "Cycle subtitles", t).clicked() { app.cycle_captions(); }
            icon_captions(&p, c, white);

            let c = pos2(right - 115.0, row_y);
            if hit(ui, c, "ontop", "Always on top", t).clicked() { app.toggle_on_top(ctx); }
            icon_compact(&p, c, tint(app.on_top));

            let c = pos2(right - 71.0, row_y);
            if hit(ui, c, "fs", "Full screen (F)", t).clicked() { app.toggle_fullscreen(ctx); }
            icon_fullscreen(&p, c, white);

            // "More" → playback speed menu
            let c = pos2(right - 27.0, row_y);
            let more = hit(ui, c, "more", "Playback speed", t);
            icon_more(&p, c, white);
            let popup_id = egui::Id::new(MORE_POPUP);
            if more.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_above_or_below_widget(ui, popup_id, &more, egui::AboveOrBelow::Above, |ui| {
                ui.set_min_width(110.0);
                ui.label("Playback speed");
                for s in [0.5_f64, 0.75, 1.0, 1.25, 1.5, 2.0] {
                    if ui.selectable_label((app.speed - s).abs() < 1e-6, format!("{s}x")).clicked() {
                        app.set_speed(s);
                        ui.memory_mut(|m| m.close_popup());
                    }
                }
            });
        });
}

// ── Folder navigation ─────────────────────────────────────────────────────────
/// Video files in the same folder as `path`, in Explorer-like (natural) order
fn scan_folder(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else { return vec![path.to_owned()] };
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && is_video_file(&p.to_string_lossy()))
                .collect()
        })
        .unwrap_or_default();
    files.sort_by(|a, b| {
        let name = |p: &PathBuf| p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        natural_cmp(&name(a), &name(b))
    });
    files
}

/// Case-insensitive compare where digit runs compare by value ("2" < "10"),
/// like Windows Explorer
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                if x.is_ascii_digit() && y.is_ascii_digit() {
                    let mut na = String::new();
                    while let Some(&c) = ai.peek() {
                        if c.is_ascii_digit() { na.push(c); ai.next(); } else { break; }
                    }
                    let mut nb = String::new();
                    while let Some(&c) = bi.peek() {
                        if c.is_ascii_digit() { nb.push(c); bi.next(); } else { break; }
                    }
                    let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                    let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                    if o != Ordering::Equal { return o; }
                } else {
                    let o = x.to_lowercase().collect::<String>()
                        .cmp(&y.to_lowercase().collect::<String>());
                    if o != Ordering::Equal { return o; }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

// ── Window edge resizing ──────────────────────────────────────────────────────
/// Which resize edge/corner (if any) the pointer is on, with its cursor icon
fn hit_edge(p: Pos2, screen: Rect) -> Option<(egui::viewport::ResizeDirection, egui::CursorIcon)> {
    use egui::viewport::ResizeDirection as D;
    use egui::CursorIcon as C;
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 14.0;

    // Keep the close/maximize/minimize buttons clickable in the top-right corner
    if p.y < WIN_BTN_H && p.x > screen.right() - WIN_BTN_W * 3.0 {
        return None;
    }

    let (dl, dr) = (p.x - screen.left(), screen.right() - p.x);
    let (dt, db) = (p.y - screen.top(), screen.bottom() - p.y);

    if dl < CORNER && dt < CORNER { return Some((D::NorthWest, C::ResizeNwSe)); }
    if dr < CORNER && dt < CORNER { return Some((D::NorthEast, C::ResizeNeSw)); }
    if dl < CORNER && db < CORNER { return Some((D::SouthWest, C::ResizeNeSw)); }
    if dr < CORNER && db < CORNER { return Some((D::SouthEast, C::ResizeNwSe)); }
    if dl < EDGE { return Some((D::West,  C::ResizeHorizontal)); }
    if dr < EDGE { return Some((D::East,  C::ResizeHorizontal)); }
    if dt < EDGE { return Some((D::North, C::ResizeVertical)); }
    if db < EDGE { return Some((D::South, C::ResizeVertical)); }
    None
}

fn fmt_time(secs: f64) -> String {
    let s = if secs.is_finite() && secs > 0.0 { secs as u64 } else { 0 };
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
            .with_min_inner_size([480.0, 300.0])
            // We draw our own title bar and window buttons
            .with_decorations(false)
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
