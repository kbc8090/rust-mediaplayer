use eframe::egui;
use libmpv2::Mpv;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{Arc, Mutex};
use std::fs::OpenOptions;
use std::io::Write;

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
        log_milestone("Initializing libmpv2 core instance...");
        
        let mpv = Mpv::new().expect("Failed to initialize libmpv backend!");
        
        // Native low-latency system parameters
        let _ = mpv.set_property("hwdec", "auto"); 
        let _ = mpv.set_property("keep-open", "yes"); 
        let _ = mpv.set_property("osc", "no"); 

        // --- FIX FULLSCREEN BLACKOUT ---
        // Force borderless windowed streaming; stops exclusive driver hijack
        let _ = mpv.set_property("fullscreen", "no");
        let _ = mpv.set_property("ontop", "no");

        let args: Vec<String> = std::env::args().collect();
        let mut initial_file = String::new();
        let mut initial_playback_state = false;

        if args.len() > 1 {
            initial_file = args[1].clone();
            initial_playback_state = true;
        }

        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            current_file: initial_file,
            is_playing: initial_playback_state,
            is_fullscreen: false,
            mpv_initialized: false,
        }
    }

    fn apply_fluent_styling(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = egui::Rounding::same(4.0);
        visuals.widgets.noninteractive.rounding = egui::Rounding::same(4.0);
        visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
        
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(31, 31, 31);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 45, 45);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(60, 60, 60);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(25, 25, 25);
        
        visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(240, 240, 240);
        visuals.widgets.hovered.fg_stroke.color = egui::Color32::WHITE;
        visuals.widgets.active.fg_stroke.color = egui::Color32::WHITE;
        visuals.widgets.noninteractive.fg_stroke.color = egui::Color32::from_rgb(200, 200, 200);
        
        visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 212);

        let mut style = (*ctx.style()).clone();
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.visuals = visuals;
        ctx.set_style(style);
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        Self::apply_fluent_styling(ctx);

        // Runtime File Dropping Handler
        ctx.input(|i| {
            if !i.raw.dropped_files.is_empty() {
                if let Some(file) = i.raw.dropped_files.first() {
                    if let Some(path) = &file.path {
                        let path_str = path.to_string_lossy().into_owned();
                        self.current_file = path_str;
                        let mpv = self.mpv.lock().unwrap();
                        let _ = mpv.command("loadfile", &[&self.current_file]);
                        self.is_playing = true;
                        let _ = mpv.set_property("pause", false);
                    }
                }
            }
        });

        // Initialize Window Handle
        if !self.mpv_initialized {
            if let Ok(handle) = frame.window_handle() {
                if let RawWindowHandle::Win32(win_handle) = handle.as_raw() {
                    let hwnd = win_handle.hwnd.get() as i64;
                    let mpv = self.mpv.lock().unwrap();
                    let _ = mpv.set_property("wid", hwnd);
                    self.mpv_initialized = true;

                    if !self.current_file.is_empty() {
                        let _ = mpv.command("loadfile", &[&self.current_file]);
                        let _ = mpv.set_property("pause", false);
                    }
                }
            }
        }

        let screen_rect = ctx.screen_rect();
        
        // --- Fullscreen Double Click Sensor Base Layer ---
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let background_response = ui.allocate_rect(ui.max_rect(), egui::Sense::click());
                if background_response.double_clicked() {
                    self.is_fullscreen = !self.is_fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                }
            });

        // Determine control panel visibility rule
        let mut show_controls = true;
        if self.is_fullscreen {
            show_controls = false;
            if let Some(mouse_pos) = ctx.pointer_latest_pos() {
                if screen_rect.max.y - mouse_pos.y <= 45.0 {
                    show_controls = true;
                }
            }
        }

        // --- FIX GHOST CONTROLS VIA FOREGROUND AREA LAYER ---
        if show_controls {
            // Force this container directly onto the foreground pixel layer
            egui::Area::new(egui::Id::new("control_ribbon_layer"))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::pos2(screen_rect.min.x, screen_rect.max.y - 36.0))
                .show(ctx, |ui| {
                    ui.allocate_ui(egui::vec2(screen_rect.width(), 36.0), |ui| {
                        egui::Frame::default()
                            .fill(egui::Color32::from_rgb(25, 25, 25))
                            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(45, 45, 45)))
                            .show(ui, |ui| {
                                ui.horizontal_centered(|ui| {
                                    let play_btn_label = if self.is_playing { "Pause" } else { "Play" };
                                    if ui.button(play_btn_label).clicked() {
                                        self.is_playing = !self.is_playing;
                                        let mpv = self.mpv.lock().unwrap();
                                        let _ = mpv.set_property("pause", !self.is_playing);
                                    }

                                    if ui.button("Stop").clicked() {
                                        let mpv = self.mpv.lock().unwrap();
                                        let _ = mpv.command("stop", &[]);
                                        self.is_playing = false;
                                    }

                                    ui.add_space(4.0);
                                    ui.separator();
                                    ui.add_space(4.0);

                                    let text_edit = egui::TextEdit::singleline(&mut self.current_file)
                                        .hint_text("Drop video here or type path...");
                                    ui.add_sized([ui.available_width() - 70.0, 22.0], text_edit);
                                    
                                    if ui.button("Load").clicked() {
                                        let mpv = self.mpv.lock().unwrap();
                                        let _ = mpv.command("loadfile", &[&self.current_file]);
                                        self.is_playing = true;
                                        let _ = mpv.set_property("pause", false);
                                    }
                                });
                            });
                    });
                });
        }

        // Keep UI thread rendering synchronized with window execution loops
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
