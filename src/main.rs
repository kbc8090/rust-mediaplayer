use eframe::egui;
use libmpv::{Format, Mpv};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{Arc, Mutex};

struct FluentMediaPlayer {
    mpv: Arc<Mutex<Mpv>>,
    current_file: String,
    is_playing: bool,
    is_fullscreen: bool,
    mpv_initialized: bool,
}

impl FluentMediaPlayer {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mpv = Mpv::new().expect("Failed to initialize libmpv backend");
        
        // Optimize MPV parameters for performance and clean UI handling
        mpv.set_property("hwdec", "auto").unwrap(); // Native GPU acceleration
        mpv.set_property("keep-open", "yes").unwrap(); // Don't crash window on file EOF
        mpv.set_property("osc", "no").unwrap(); // Disable default built-in mpv controls

        Self {
            mpv: Arc::new(Mutex::new(mpv)),
            current_file: String::new(),
            is_playing: false,
            is_fullscreen: false,
            mpv_initialized: false,
        }
    }

    fn apply_fluent_styling(ctx: &egui::Context) {
        let mut style = (*ctx.style()).clone();
        
        // Fluent Geometry (Windows 10/11 crisp compact layout)
        style.visuals.window_rounding = egui::Rounding::same(4.0);
        style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(4.0);
        style.visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
        style.spacing.button_padding = egui::vec2(12.0, 4.0);
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);

        // Windows Dark Theme Palette Match
        style.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(31, 31, 31);
        style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(45, 45, 45);
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(55, 55, 55);
        style.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(28, 28, 28);
        
        // System Blue Accent Color
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 212);
        style.visuals.widgets.hovered.text_color = egui::Color32::WHITE;

        ctx.set_style(style);
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        Self::apply_fluent_styling(ctx);

        // Hook MPV direct rendering to window on initial layout
        if !self.mpv_initialized {
            if let Ok(handle) = frame.window_handle() {
                if let RawWindowHandle::Win32(win_handle) = handle.as_raw() {
                    let hwnd = win_handle.hwnd.get() as i64;
                    let mpv = self.mpv.lock().unwrap();
                    mpv.set_property("wid", hwnd).unwrap();
                    self.mpv_initialized = true;
                }
            }
        }

        let screen_rect = ctx.screen_rect();
        let mut toggle_fullscreen = false;

        // --- Fullscreen Double Click Handler ---
        let background_response = ctx.allocate_rect(screen_rect, egui::Sense::click());
        if background_response.double_clicked() {
            toggle_fullscreen = true;
        }

        if toggle_fullscreen {
            self.is_fullscreen = !self.is_fullscreen;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
        }

        // --- Low-Latency Hover Control Detection ---
        let mut show_controls = true;
        
        if self.is_fullscreen {
            show_controls = false;
            if let Some(mouse_pos) = ctx.pointer_latest_pos() {
                // Instant activation boundary checks within 20px from screen bottom
                if screen_rect.max.y - mouse_pos.y <= 20.0 {
                    show_controls = true;
                }
            }
        }

        // --- Compact Bottom Controls Ribbon (34px tall) ---
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
                            mpv.set_property("pause", !self.is_playing).unwrap();
                        }

                        if ui.button("⏹").clicked() {
                            let mpv = self.mpv.lock().unwrap();
                            mpv.command("stop", &[]).unwrap();
                            self.is_playing = false;
                        }

                        ui.add_space(4.0);
                        ui.separator();
                        ui.add_space(4.0);

                        // Lean URL / Directory Input Field
                        let text_edit = egui::TextEdit::singleline(&mut self.current_file)
                            .hint_text("Enter file path or URL stream...");
                        ui.add_sized([ui.available_width() - 80.0, 22.0], text_edit);
                        
                        if ui.button("Load").clicked() {
                            let mpv = self.mpv.lock().unwrap();
                            mpv.command("loadfile", &[&self.current_file]).unwrap();
                            self.is_playing = true;
                            mpv.set_property("pause", false).unwrap();
                        }
                    });
                });
        }

        // Always repaint immediately to ensure prompt mouse tracking responsiveness
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
        Box::new(|cc| Ok(Box::new(FluentMediaPlayer::new(cc)))),
    )
}