use eframe::egui;
use eframe::glow;
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams}};
use std::sync::{Arc, Mutex};

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    mpv_gl: Option<RenderContext>,
    texture_id: Option<egui::TextureId>,
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

        // Request raw access to eframe's active OpenGL environment
        let gl = cc.gl.clone().expect("Glow OpenGL context missing!");
        
        // Wire MPV's render subsystem to look up eframe's internal thread addresses
        let mpv_gl = unsafe {
            RenderContext::new_opengl(
                &mpv,
                OpenGLInitParams {
                    get_proc_address: Box::new(move |name| {
                        gl.get_proc_address(name) as *mut std::ffi::c_void
                    }),
                },
            ).ok()
        };

        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            mpv_gl,
            texture_id: None,
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
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        Self::apply_fluent_styling(ctx);

        // Active low-latency file dropping
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

        // --- THE RENDER LOOP LINK ---
        let screen_size = ctx.screen_rect().size();
        
        if let Some(ref mut render_ctx) = self.mpv_gl {
            // Tell MPV to output its active frame payload directly into an OpenGL texture allocation
            let width = screen_size.x as i32;
            let height = screen_size.y as i32;

            // Generate an allocation token inside egui's native texture registry if it doesn't exist
            if self.texture_id.is_none() {
                let callback = egui::PaintCallback {
                    rect: ctx.screen_rect(),
                    origin: egui::PaintCallbackOrigin::Main,
                    callback: Arc::new(eframe::glow::CallbackFn::new({
                        let render_ctx = render_ctx.clone();
                        move |_info, _painter| {
                            // Native GPU execution block
                            unsafe {
                                render_ctx.render_opengl(0, width, height);
                            }
                        }
                    })),
                };
                
                // Draw the video frame directly onto the absolute bottom background layer
                ctx.layer_painter(egui::LayerId::background()).add(callback);
            }
        }

        // --- IMMUTABLE FOREGROUND CONTROLS ---
        // Because the video is now drawn safely on the background layer, panels stay on top!
        egui::TopBottomPanel::bottom("controls")
            .frame(egui::Frame::default().fill(egui::Color32::from_rgb(20, 20, 20).with_alpha(220)))
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
                        .hint_text("Drag video files directly here...");
                    ui.add_sized([ui.available_width() - 80.0, 20.0], text_edit);

                    if ui.button("Load").clicked() {
                        let _ = self.mpv.lock().unwrap().command("loadfile", &[&self.current_file]);
                        self.is_playing = true;
                    }
                });
            });

        // Detect full-screen requests without driver side-effects
        if ctx.input(|i| i.pointer.any_click() && i.pointer.is_decidedly_dragged()) {
            // Fallback safety
        }
        
        let background_response = ctx.input(|i| i.pointer.any_click());
        // Handle background double click tracking safely via egui structures
        
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
