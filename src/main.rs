use eframe::egui;
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams, RenderParam, RenderParamApiType}};
use std::sync::{Arc, Mutex};
use std::cell::RefCell;
use std::path::Path;

// Raw Win32 OpenGL function pointer loader hooks
unsafe extern "system" {
    fn wglGetProcAddress(lpszProc: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(lpModuleName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}

thread_local! {
    static TLS_RENDER_CTX: RefCell<Option<RenderContext<'static>>> = const { RefCell::new(None) };
}

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    render_ctx_initialized: bool,
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
    
    // Playback state tracking for Fluent overlay UI
    time_pos: f64,
    duration: f64,
    volume: i64,
    show_controls: bool,
    last_mouse_move: f64,
}

impl FluentMediaPlayer {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mpv = Mpv::new().expect("Failed to initialize libmpv backend!");
        let _ = mpv.set_property("hwdec", "auto");
        let _ = mpv.set_property("keep-open", "yes");
        let _ = mpv.set_property("osc", "no");

        Self::configure_fonts(&cc.egui_ctx);
        Self::apply_fluent_styling(&cc.egui_ctx);

        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            render_ctx_initialized: false,
            current_file: String::new(),
            is_playing: false,
            is_fullscreen: false,
            time_pos: 0.0,
            duration: 0.0,
            volume: 100,
            show_controls: true,
            last_mouse_move: 0.0,
        }
    }

    fn configure_fonts(ctx: &egui::Context) {
        let mut definitions = egui::FontDefinitions::default();
        let segoe_path = Path::new("C:\\Windows\\Fonts\\segoeui.ttf");
        let icon_path = Path::new("C:\\Windows\\Fonts\\SegoeIcons.ttf");

        if segoe_path.exists() {
            if let Ok(data) = std::fs::read(segoe_path) {
                definitions.font_data.insert("SegoeUI".to_owned(), egui::FontData::from_owned(data));
                definitions.families.get_mut(&egui::FontFamily::Proportional).unwrap().insert(0, "SegoeUI".to_owned());
            }
        }
        if icon_path.exists() {
            if let Ok(data) = std::fs::read(icon_path) {
                definitions.font_data.insert("FluentIcons".to_owned(), egui::FontData::from_owned(data));
                definitions.families.get_mut(&egui::FontFamily::Proportional).unwrap().push("FluentIcons".to_owned());
            }
        }
        ctx.set_fonts(definitions);
    }

    fn apply_fluent_styling(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = egui::Rounding::same(8.0);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgba_premultiplied(26, 26, 26, 240);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgba_premultiplied(45, 45, 45, 180);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgba_premultiplied(55, 55, 55, 220);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgba_premultiplied(35, 35, 35, 250);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(15));
        visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 212);

        let mut style = (*ctx.style()).clone();
        style.spacing.button_padding = egui::vec2(16.0, 8.0);
        style.visuals = visuals;
        ctx.set_style(style);
    }

    fn format_time(seconds: f64) -> String {
        if seconds.is_nan() || seconds.is_infinite() { return "00:00".to_string(); }
        let total_secs = seconds.round() as i64;
        format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Ok(mpv) = self.mpv.lock() {
            if let Ok(t) = mpv.get_property::<f64>("time-pos") { self.time_pos = t; }
            if let Ok(d) = mpv.get_property::<f64>("duration") { self.duration = d; }
            if let Ok(v) = mpv.get_property::<i64>("volume") { self.volume = v; }
        }

        if !self.render_ctx_initialized {
            if frame.gl().is_some() {
                let init_params = OpenGLInitParams {
                    get_proc_address: |_, name| unsafe {
                        let c_name = std::ffi::CString::new(name).unwrap();
                        let addr = wglGetProcAddress(c_name.as_ptr());
                        if !addr.is_null() && addr as usize != 1 && addr as usize != 2 && addr as usize != 3 && addr as usize != !0 { return addr; }
                        let h_module = GetModuleHandleA(b"opengl32.dll\0".as_ptr() as *const std::os::raw::c_char);
                        if !h_module.is_null() { return GetProcAddress(h_module, c_name.as_ptr()); }
                        std::ptr::null_mut()
                    },
                    ctx: std::ptr::null_mut::<std::os::raw::c_void>(),
                };
                let params = vec![RenderParam::ApiType(RenderParamApiType::OpenGl), RenderParam::InitParams(init_params)];
                let mpv = self.mpv.lock().unwrap();
                let mpv_ref: &'static Mpv = unsafe { std::mem::transmute(&*mpv) };
                if let Ok(rc) = mpv_ref.create_render_context(params) {
                    TLS_RENDER_CTX.with(|cell| { *cell.borrow_mut() = Some(rc); });
                    self.render_ctx_initialized = true;
                }
            }
        }

        ctx.input(|i| {
            if let Some(file) = i.raw.dropped_files.first() {
                if let Some(path) = &file.path {
                    self.current_file = path.to_string_lossy().into_owned();
                    let _ = self.mpv.lock().unwrap().command("loadfile", &[&self.current_file]);
                    self.is_playing = true;
                }
            }
            if i.pointer.delta().length_sq() > 0.0 {
                self.last_mouse_move = i.time;
                self.show_controls = true;
            } else if i.time - self.last_mouse_move > 2.5 && self.is_fullscreen {
                self.show_controls = false;
            }
        });

        egui::CentralPanel::default().frame(egui::Frame::none().fill(egui::Color32::BLACK)).show(ctx, |ui| {
            let rect = ui.max_rect();
            let response = ui.allocate_rect(rect, egui::Sense::click());
            if response.double_clicked() {
                self.is_fullscreen = !self.is_fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                TLS_RENDER_CTX.with(|cell| {
                    if let Some(ref mut rc) = *cell.borrow_mut() {
                        let _ = rc.render::<*mut std::os::raw::c_void>(0, rect.width() as i32, rect.height() as i32, true);
                    }
                });
            } else if response.clicked() {
                self.is_playing = !self.is_playing;
                let _ = self.mpv.lock().unwrap().set_property("pause", !self.is_playing);
            }
            if self.render_ctx_initialized {
                let (w, h) = (rect.width() as i32, rect.height() as i32);
                ui.painter().add(egui::PaintCallback {
                    rect,
                    callback: Arc::new(egui_glow::CallbackFn::new(move |_info, _painter| {
                        TLS_RENDER_CTX.with(|cell| {
                            if let Some(ref mut rc) = *cell.borrow_mut() {
                                let _ = rc.render::<*mut std::os::raw::c_void>(0, w, h, false);
                            }
                        });
                    })),
                });
            }
        });

        if self.show_controls || !self.is_fullscreen {
            egui::TopBottomPanel::bottom("fluent_ribbon")
                .frame(egui::Frame::default().fill(egui::Color32::from_rgba_premultiplied(20, 20, 20, 215)).inner_margin(egui::Margin::symmetric(24.0, 14.0)))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(Self::format_time(self.time_pos));
                        let mut progress = self.time_pos;
                        ui.spacing_mut().slider_width = ui.available_width() - 55.0;
                        if ui.add(egui::Slider::new(&mut progress, 0.0..=self.duration.max(1.0)).show_value(false).trailing_fill(true)).changed() {
                            let _ = self.mpv.lock().unwrap().set_property("time-pos", progress);
                        }
                        ui.label(Self::format_time(self.duration));
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button(if self.is_playing { "\u{E103}" } else { "\u{E102}" }).clicked() {
                            self.is_playing = !self.is_playing;
                            let _ = self.mpv.lock().unwrap().set_property("pause", !self.is_playing);
                        }
                        if ui.button("\u{E15B}").clicked() {
                            let _ = self.mpv.lock().unwrap().command("stop", &[]);
                            self.is_playing = false;
                        }
                        ui.separator();
                        let mut vol = self.volume as f32;
                        ui.spacing_mut().slider_width = 80.0;
                        if ui.add(egui::Slider::new(&mut vol, 0.0..=100.0).show_value(false)).changed() {
                            let _ = self.mpv.lock().unwrap().set_property("volume", vol as i64);
                        }
                    });
                });
        }
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    eframe::run_native("Media Player", eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([960.0, 540.0]).with_transparent(true),
        ..Default::default()
    }, Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))))
}
