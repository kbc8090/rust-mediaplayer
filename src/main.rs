use eframe::egui;
// Pull glow directly from eframe's re-exports where it actually lives
use eframe::glow; 
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams}};
use std::sync::{Arc, Mutex};

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    // Use an Arc wrapper so we can safely share the MPV context with the UI paint thread
    mpv_gl: Option<Arc<Mutex<RenderContext>>>, 
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
}

impl FluentMediaPlayer {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mpv = Mpv::new().expect("Failed to initialize libmpv backend!");
        let _ = mpv.set_property("hwdec", "auto");
        let _ = mpv.set_property("keep-open", "yes");
        let _ = mpv.set_property("osc", "no");

        // Access the raw GL context provided by eframe
        let mpv_gl = if let Some(gl) = &cc.gl {
            let gl_clone = gl.clone();
            
            // In libmpv2, the API uses init_opengl directly on the instantiated context
            let render_param = OpenGLInitParams {
                get_proc_address: Box::new(move |name| {
                    // glow contexts use standard lookup syntax via your hardware drivers
                    gl_clone.get_proc_address(name) as *mut std::ffi::c_void
                }),
            };

            // Safely initialize the internal GPU renderer context
            unsafe {
                RenderContext::init_opengl(&mpv, render_param)
                    .ok()
                    .map(|ctx| Arc::new(Mutex::new(ctx)))
            }
        } else {
            None
        };

        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            mpv_gl,
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

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        Self::apply_fluent_styling(ctx);

        // Process drag and drop assets
        ctx.input(|i| {
            if let Some(file) = i.raw.dropped_files.first() {
                if let Some(path) = &file.path {
                    self.current_file = path.to_string_lossy().into_owned();
                    let mpv = self.mpv.lock().unwrap();
                    let _ = mpv.command("loadfile", &[&self.current_file]);
                    self.is_playing = true;
                }
            }
        });

        // Main UI Frame container layout
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let response = ui.allocate_rect(rect, egui::Sense::click());
                if response.double_clicked() {
                    self.is_fullscreen = !self.is_fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                }

                // Push frame buffer data down onto the active layout texture surface
                if let Some(ref render_ctx_mutex) = self.mpv_gl {
                    let width = rect.width() as i32;
                    let height = rect.height() as i32;
                    let render_ctx_clone = render_ctx_mutex.clone();

                    let callback = egui::PaintCallback {
                        rect,
                        callback: Arc::new(glow::CallbackFn::new(move |_info, _painter| {
                            if let Ok(mut render_ctx) = render_ctx_clone.lock() {
                                unsafe {
                                    // Target FBO 0 (Default display frame buffer array)
                                    let _ = render_ctx.render_opengl(0, width, height);
                                }
                            }
                        })),
                    };
                    ui.painter().add(callback);
                }
            });

        // Bottom UI playback navigation strip panel overlay
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
                            .hint_text("Drag video files here or paste path...");
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
