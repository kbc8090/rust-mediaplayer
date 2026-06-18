use eframe::egui;
use libmpv2::{Mpv, render::{RenderContext, OpenGLInitParams}};
use std::sync::{Arc, Mutex};

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    // We store the context in an Option; we will initialize it during the first frame
    render_ctx: Option<RenderContext>, 
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
}

impl FluentMediaPlayer {
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
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // One-time initialization of the RenderContext using the frame's GL pointer
        if self.render_ctx.is_none() {
            if let Some(gl) = frame.gl() {
                let mpv = self.mpv.lock().unwrap();
                let params = OpenGLInitParams {
                    get_proc_address: Box::new(move |name| {
                        gl.get_proc_address(name) as *mut std::ffi::c_void
                    }),
                };
                self.render_ctx = unsafe { RenderContext::new(&mpv, params).ok() };
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
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.max_rect();
            if let Some(ref mut rc) = self.render_ctx {
                // Using the modern RenderContext trait approach
                let width = rect.width() as i32;
                let height = rect.height() as i32;
                
                let cb = egui::PaintCallback {
                    rect,
                    callback: Arc::new(egui::load::Bytes::from_static(&[])), // Placeholder
                };
                // Real rendering call
                unsafe { rc.render(0, width, height); }
            }
        });
        
        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "Media Player",
        eframe::NativeOptions::default(),
        Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))),
    )
}
