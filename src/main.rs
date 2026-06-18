use eframe::egui;
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams}};
use std::sync::{Arc, Mutex};

// Raw Win32 OpenGL function pointer loader hooks
unsafe extern "system" {
    fn wglGetProcAddress(lpszProc: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetModuleHandleA(lpModuleName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
    fn GetProcAddress(hModule: *mut std::ffi::c_void, lpProcName: *const std::os::raw::c_char) -> *mut std::ffi::c_void;
}

struct FluentMediaPlayer<'a> {
    mpv: Arc<Mutex<Mpv>>,
    render_ctx: Option<RenderContext<'a>>, 
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
}

impl<'a> FluentMediaPlayer<'a> {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mpv = Mpv::new().expect("Failed to initialize libmpv backend!");
        let _ = mpv.set_property("hwdec", "auto");
        let _ = mpv.set_property("keep-open", "yes");
        let _ = mpv.set_property("osc", "no");

        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            render_ctx: None,
            current_file: String::new(),
            is_playing: false,
            is_fullscreen: false,
        }
    }

    fn apply_fluent_styling(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = egui::Rounding::same(4.0);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(31, 31, 31);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 45, 45);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(60, 60, 60);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(25, 25, 25);
        
        visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(240, 240, 240);
        visuals.widgets.hovered.fg_stroke.color = egui::Color32::WHITE;
        visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 212);

        let mut style = (*ctx.style()).clone();
        style.spacing.button_padding = egui::vec2(12.0, 5.0);
        style.visuals = visuals;
        ctx.set_style(style);
    }
}

impl<'a> eframe::App for FluentMediaPlayer<'a> {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        Self::apply_fluent_styling(ctx);

        // One-time hardware initialization of the MPV RenderContext
        if self.render_ctx.is_none() {
            if frame.gl().is_some() {
                // FIX E0308: Stateless closure accepting 2 arguments auto-coerces to a bare fn pointer
                let params = OpenGLInitParams {
                    get_proc_address: |_, name| unsafe {
                        let c_name = std::ffi::CString::new(name).unwrap();
                        let addr = wglGetProcAddress(c_name.as_ptr());
                        if !addr.is_null() && addr as usize != 1 && addr as usize != 2 && addr as usize != 3 && addr as usize != !0 {
                            return addr;
                        }
                        // Fallback to core opengl32 system driver context module
                        let h_module = GetModuleHandleA(b"opengl32.dll\0".as_ptr() as *const std::os::raw::c_char);
                        if !h_module.is_null() {
                            return GetProcAddress(h_module, c_name.as_ptr());
                        }
                        std::ptr::null_mut()
                    },
                    ctx: std::ptr::null_mut(),
                };

                let mut mpv = self.mpv.lock().unwrap();
                let render_ctx = unsafe {
                    let mpv_ptr = &mut *mpv as *mut Mpv;
                    // Safely extend lifetime mapping via raw pointer to bind context safely to the parent loop
                    let mpv_ref: &'a mut Mpv = &mut *mpv_ptr;
                    
                    // FIX E0599: Call the unified method directly on the underlying Mpv instance context
                    mpv_ref.render_context(params).ok()
                };
                
                if let Some(rc) = render_ctx {
                    self.render_ctx = Some(rc);
                }
            }
        }

        // Drag and drop asset registration
        ctx.input(|i| {
            if let Some(file) = i.raw.dropped_files.first() {
                if let Some(path) = &file.path {
                    self.current_file = path.to_string_lossy().into_owned();
                    let _ = self.mpv.lock().unwrap().command("loadfile", &[&self.current_file]);
                    self.is_playing = true;
                }
            }
        });

        // Main Video Canvas Interface Display
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                
                let response = ui.allocate_rect(rect, egui::Sense::click());
                if response.double_clicked() {
                    self.is_fullscreen = !self.is_fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                }

                if let Some(ref mut rc) = self.render_ctx {
                    let width = rect.width() as i32;
                    let height = rect.height() as i32;
                    
                    let rc_ptr = rc as *mut RenderContext<'a> as usize;

                    let callback = egui::PaintCallback {
                        rect,
                        callback: Arc::new(egui_glow::CallbackFn::new(move |_info, _painter| unsafe {
                            let rc_ref = &mut *(rc_ptr as *mut RenderContext<'a>);
                            let _ = rc_ref.render(0, width, height, false);
                        })),
                    };
                    ui.painter().add(callback);
                }
            });

        // Overlay control ribbon configurations
        let mut show_controls = true;
        if self.is_fullscreen {
            show_controls = false;
            if let Some(mouse_pos) = ctx.pointer_latest_pos() {
                if ctx.screen_rect().max.y - mouse_pos.y <= 50.0 {
                    show_controls = true;
                }
            }
        }

        if show_controls {
            egui::TopBottomPanel::bottom("controls_panel")
                .frame(egui::Frame::default().fill(egui::Color32::from_rgba_premultiplied(20, 20, 20, 220)))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let label = if self.is_playing { "Pause" } else { "Play" };
                        if ui.button(label).clicked() {
                            self.is_playing = !self.is_playing;
                            let _ = self.mpv.lock().unwrap().set_property("pause", !self.is_playing);
                        }

                        if ui.button("Stop").clicked() {
                            let _ = self.mpv.lock().unwrap().command("stop", &[]);
                            self.is_playing = false;
                        }

                        ui.separator();

                        let text_edit = egui::TextEdit::singleline(&mut self.current_file)
                            .hint_text("Drag video files here...");
                        ui.add_sized([ui.available_width() - 80.0, 20.0], text_edit);

                        if ui.button("Load").clicked() {
                            let _ = self.mpv.lock().unwrap().command("loadfile", &[&self.current_file]);
                            self.is_playing = true;
                        }
                    });
                });
        }

        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([850.0, 480.0])
            .with_title("Media Player"),
        ..Default::default()
    };

    eframe::run_native(
        "Media Player",
        options,
        Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))),
    )
}
