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

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    render_ctx_initialized: bool,
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
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
        let _ = mpv.set_property("loop-file", "inf");
        let _ = mpv.set_property("osc", "no");

        // Force initial frame
        cc.egui_ctx.request_repaint();

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
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Ok(mpv) = self.mpv.lock() {
            let _ = mpv.get_property("time-pos").map(|t| self.time_pos = t);
            let _ = mpv.get_property("duration").map(|d| self.duration = d);
            let _ = mpv.get_property("volume").map(|v| self.volume = v);
        }

        if !self.render_ctx_initialized && frame.gl().is_some() {
            let init_params = OpenGLInitParams {
                get_proc_address: |_, name| unsafe {
                    let c_name = std::ffi::CString::new(name).unwrap();
                    let addr = wglGetProcAddress(c_name.as_ptr());
                    if !addr.is_null() && (addr as usize) > 3 && (addr as usize) != !0 { return addr; }
                    let h_mod = GetModuleHandleA(b"opengl32.dll\0".as_ptr() as *const _);
                    GetProcAddress(h_mod, c_name.as_ptr())
                },
                ctx: std::ptr::null_mut::<std::os::raw::c_void>(),
            };
            let params = vec![RenderParam::ApiType(RenderParamApiType::OpenGl), RenderParam::InitParams(init_params)];
            if let Ok(rc) = unsafe { std::mem::transmute::<&Mpv, &'static Mpv>(&*self.mpv.lock().unwrap()) }.create_render_context(params) {
                TLS_RENDER_CTX.with(|c| *c.borrow_mut() = Some(rc));
                self.render_ctx_initialized = true;
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
            let mouse_y = i.pointer.hover_pos().map(|p| p.y).unwrap_or(0.0);
            let screen_h = ctx.screen_rect().height();
            if i.pointer.delta().length_sq() > 0.0 || (self.is_fullscreen && screen_h - mouse_y < 20.0) {
                self.last_mouse_move = i.time;
                self.show_controls = true;
            } else if i.time - self.last_mouse_move > 2.5 {
                self.show_controls = false;
            }
        });

        egui::CentralPanel::default().frame(egui::Frame::none().fill(egui::Color32::BLACK)).show(ctx, |ui| {
            let rect = ui.max_rect();
            if ui.allocate_rect(rect, egui::Sense::click()).double_clicked() {
                self.is_fullscreen = !self.is_fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
            }
            if self.render_ctx_initialized {
                let (w, h) = (rect.width() as i32, rect.height() as i32);
                ui.painter().add(egui::PaintCallback { rect, callback: Arc::new(egui_glow::CallbackFn::new(move |_, _| {
                    TLS_RENDER_CTX.with(|c| if let Some(rc) = c.borrow_mut().as_mut() {
                        let _ = rc.render::<*mut std::os::raw::c_void>(0, w, h, false);
                    });
                }))});
            }
        });

        if self.show_controls {
            egui::TopBottomPanel::bottom("ribbon").frame(egui::Frame::default()
                .fill(egui::Color32::from_black_alpha(220)).inner_margin(6.0)).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(if self.is_playing { "\u{E103}" } else { "\u{E102}" }).clicked() {
                        self.is_playing = !self.is_playing;
                        let _ = self.mpv.lock().unwrap().set_property("pause", !self.is_playing);
                    }
                    ui.spacing_mut().slider_width = ui.available_width() - 120.0;
                    let mut progress = self.time_pos;
                    if ui.add(egui::Slider::new(&mut progress, 0.0..=self.duration.max(1.0)).show_value(false).trailing_fill(true)).changed() {
                        let _ = self.mpv.lock().unwrap().set_property("time-pos", progress);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let mut vol = self.volume as f32;
                        ui.spacing_mut().slider_width = 60.0;
                        if ui.add(egui::Slider::new(&mut vol, 0.0..=100.0).show_value(false)).changed() {
                            let _ = self.mpv.lock().unwrap().set_property("volume", vol as i64);
                        }
                        ui.label("\u{E15D}");
                    });
                });
            });
        }
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 540.0])
            .with_min_inner_size([400.0, 300.0])
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native("Media Player", options, Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))))
}
