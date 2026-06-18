use eframe::egui;
use libmpv2::Mpv;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{Arc, Mutex};
use std::fs::OpenOptions;
use std::io::Write;

// High-performance helper to log system milestones to disk
fn log_milestone(message: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("player_debug.log") {
        let _ = writeln!(file, "[SYSTEM LOG] {}", message);
    }
}

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
    mpv_initialized: bool,
}

impl FluentMediaPlayer {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        log_milestone("Initializing libmpv core instance...");
        
        let mpv = Mpv::new().expect("Failed to initialize libmpv backend. Ensure mpv-1.dll is in the folder!");
        
        // Safe property assignments: We drop .unwrap() to prevent soft failures from causing silent panics
        let _ = mpv.set_property("hwdec", "auto"); 
        let _ = mpv.set_property("keep-open", "yes"); 
        let _ = mpv.set_property("osc", "no"); 

        let args: Vec<String> = std::env::args().collect();
        let mut initial_file = String::new();
        let mut initial_playback_state = false;

        if args.len() > 1 {
            initial_file = args[1].clone();
            initial_playback_state = true;
            log_milestone(&format!("Caught launch argument file: {}", initial_file));
        }

        log_milestone("FluentMediaPlayer state constructed successfully.");
        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            current_file: initial_file,
            is_playing: initial_playback_state,
            is_fullscreen: false,
            mpv_initialized: false,
        }
    }

    fn apply_fluent_styling(ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        style.visuals.window_rounding = egui::Rounding::same(4.0);
        style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(4.0);
        style.visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
        style.spacing.button_padding = egui::vec2(12.0, 4.0);
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(31, 31, 31);
        style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 45, 45);
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(55, 55, 55);
        style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(28, 28, 28);
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 212);
        style.visuals.widgets.hovered.fg_stroke.color = egui::Color32::WHITE;
        ctx.set_style(style);
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        Self::apply_fluent_styling(ctx);

        if !self.mpv_initialized {
            log_milestone("Attempting window handle acquisition...");
            if let Ok(handle) = frame.window_handle() {
                if let RawWindowHandle::Win32(win_handle) = handle.as_raw() {
                    let hwnd = win_handle.hwnd.get() as i64;
                    log_milestone(&format!("Acquired HWND: {}. Binding to MPV WID...", hwnd));
                    
                    let mpv = self.mpv.lock().unwrap();
                    // Catch errors safely instead of unwrapping
                    if let Err(err) = mpv.set_property("wid", hwnd) {
                        log_milestone(&format!("CRITICAL: Failed to bind MPV to WID: {:?}", err));
                    } else {
                        log_milestone("Successfully attached MPV rendering pipeline to window handle.");
                    }
                    
                    self.mpv_initialized = true;

                    if !self.current_file.is_empty() {
                        let _ = mpv.command("loadfile", &[&self.current_file]);
                        let _ = mpv.set_property("pause", false);
                    }
                }
            }
        }

        let screen_rect = ctx.screen_rect();
        let mut show_controls = true;
        if self.is_fullscreen {
            show_controls = false;
            if let Some(mouse_pos) = ctx.pointer_latest_pos() {
                if screen_rect.max.y - mouse_pos.y <= 20.0 {
                    show_controls = true;
                }
            }
        }

        if show_controls {
            egui::TopBottomPanel::bottom("fluent_ribbon")
                .exact_height(34.0)
                .frame(egui::Frame::default()
                    .fill(egui::Color32::from_rgb(25, 25, 25))
                    .inner_margin(egui::Margin::symmetric(12.0, 4.0))
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 40, 40))))
                .show(ctx, |ui| {
                    ui.horizontal_centered(|ui| {
                        let play_btn_label = if self.is_playing { "⏸" } else { "▶" };
                        if ui.button(play_btn_label).clicked() {
                            self.is_playing = !self.is_playing;
                            let mpv = self.mpv.lock().unwrap();
                            let _ = mpv.set_property("pause", !self.is_playing);
                        }

                        if ui.button("⏹").clicked() {
                            let mpv = self.mpv.lock().unwrap();
                            let _ = mpv.command("stop", &[]);
                            self.is_playing = false;
                        }

                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);

                        let text_edit = egui::TextEdit::singleline(&mut self.current_file)
                            .hint_text("Enter file path or URL stream...");
                        ui.add_sized([ui.available_width() - 80.0, 22.0], text_edit);
                        
                        if ui.button("Load").clicked() {
                            let mpv = self.mpv.lock().unwrap();
                            let _ = mpv.command("loadfile", &[&self.current_file]);
                            self.is_playing = true;
                            let _ = mpv.set_property("pause", false);
                        }
                    });
                });
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let background_response = ui.allocate_rect(ui.max_rect(), egui::Sense::click());
                if background_response.double_clicked() {
                    self.is_fullscreen = !self.is_fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                }
            });

        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    // --- Safe Intercept Hook for Silent Panics ---
    std::panic::set_hook(Box::new(|info| {
        let msg = match info.payload().downcast_ref::<&str>() {
            Some(s) => *s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => &**s,
                None => "Unhandled runtime panic exception context.",
            },
        };
        let location = info.location().map(|l| format!("at {}:{}", l.file(), l.line())).unwrap_or_default();
        let log_text = format!("=== CRASH LOG ===\nApplication Panicked!\nMessage: {}\nLocation: {}\n", msg, location);
        std::fs::write("player_crash_log.txt", log_text).ok();
    }));

    log_milestone("Entering main engine entry hook.");

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
