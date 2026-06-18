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
        
        // Load native Segoe UI variants directly from Windows system directories
        let segoe_path = Path::new("C:\\Windows\\Fonts\\segoeui.ttf");
        let icon_path = Path::new("C:\\Windows\\Fonts\\SegoeIcons.ttf"); // Windows 11 Fluent Icons

        if segoe_path.exists() {
            if let Ok(data) = std::fs::read(segoe_path) {
                definitions.font_data.insert("SegoeUI".to_owned(), egui::FontData::from_owned(data));
                definitions.families.get_mut(&egui::FontFamily::Proportional)
                    .unwrap().insert(0, "SegoeUI".to_owned());
            }
        }

        if icon_path.exists() {
            if let Ok(data) = std::fs::read(icon_path) {
                definitions.font_data.insert("FluentIcons".to_owned(), egui::FontData::from_owned(data));
                definitions.families.get_mut(&egui::FontFamily::Proportional)
                    .unwrap().push("FluentIcons".to_owned());
            }
        }

        ctx.set_fonts(definitions);
    }

    fn apply_fluent_styling(ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();
        
        // Windows 11 Fluent Design color specs (Mica Dark Base)
        visuals.window_rounding = egui::Rounding::same(8.0);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgba_premultiplied(26, 26, 26, 240);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgba_premultiplied(45, 45, 45, 180);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgba_premultiplied(55, 55, 55, 220);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgba_premultiplied(35, 35, 35, 250);
        
        // Borders and typography
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_white_alpha(15));
        visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(243, 243, 243);
        visuals.widgets.hovered.fg_stroke.color = egui::Color32::WHITE;
        
        // Windows Accent Color (Fluent Blue)
        visuals.selection.bg_fill = egui::Color32::from_rgb(0, 120, 212);

        let mut style = (*ctx.style()).clone();
        style.spacing.button_padding = egui::vec2(16.0, 8.0);
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.visuals = visuals;
        ctx.set_style(style);
    }

    fn format_time(seconds: f64) -> String {
        if seconds.is_nan() || seconds.is_infinite() { return "00:00".to_string(); }
        let total_secs = seconds.round() as i64;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        format!("{:02}:{:02}", mins, secs)
    }
}

impl eframe::App for FluentMediaPlayer {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Sync media status parameters dynamically from libmpv core
        if let Ok(mpv) = self.mpv.lock() {
            if let Ok(t) = mpv.get_property::<f64>("time-pos") { self.time_pos = t; }
            if let Ok(d) = mpv.get_property::<f64>("duration") { self.duration = d; }
            if let Ok(v) = mpv.get_property::<i64>("volume") { self.volume = v; }
        }

        // Initialize RenderContext on first draw
        if !self.render_ctx_initialized {
            if frame.gl().is_some() {
                let init_params = OpenGLInitParams {
                    get_proc_address: |_, name| unsafe {
                        let c_name = std::ffi::CString::new(name).unwrap();
                        let addr = wglGetProcAddress(c_name.as_ptr());
                        if !addr.is_null() && addr as usize != 1 && addr as usize != 2 && addr as usize != 3 && addr as usize != !0 {
                            return addr;
                        }
                        let h_module = GetModuleHandleA(b"opengl32.dll\0".as_ptr() as *const std::os::raw::c_char);
                        if !h_module.is_null() {
                            return GetProcAddress(h_module, c_name.as_ptr());
                        }
                        std::ptr::null_mut()
                    },
                    ctx: std::ptr::null_mut::<std::os::raw::c_void>(),
                };

                let params = vec![
                    RenderParam::ApiType(RenderParamApiType::OpenGl),
                    RenderParam::InitParams(init_params),
                ];

                let mpv = self.mpv.lock().unwrap();
                let mpv_ref: &'static Mpv = unsafe { std::mem::transmute(&*mpv) };
                
                if let Ok(rc) = mpv_ref.create_render_context(params) {
                    TLS_RENDER_CTX.with(|cell| { *cell.borrow_mut() = Some(rc); });
                    self.render_ctx_initialized = true;
                }
            }
        }

        // Handle Drag & Drop
        ctx.input(|i| {
            if let Some(file) = i.raw.dropped_files.first() {
                if let Some(path) = &file.path {
                    self.current_file = path.to_string_lossy().into_owned();
                    let _ = self.mpv.lock().unwrap().command("loadfile", &[&self.current_file]);
                    self.is_playing = true;
                }
            }
            // Auto-hide controls when mouse is inactive
            if i.pointer.any_moved() {
                let mut cell = self.last_mouse_move;
                *(&mut self.last_mouse_move) = ctx.input(|i| i.time);
                *(&mut self.show_controls) = true;
            } else if ctx.input(|i| i.time) - self.last_mouse_move > 2.5 && self.is_fullscreen {
                *(&mut self.show_controls) = false;
            }
        });

        // Background Layer: Video Frame Canvas
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(egui::Color32::BLACK))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                
                // Clicking canvas toggles playback state; double clicking toggles fullscreen
                let response = ui.allocate_rect(rect, egui::Sense::click());
                if response.double_clicked() {
                    self.is_fullscreen = !self.is_fullscreen;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                    
                    // PREVENT BLACK FLASH: Force libmpv to update bounds instantly on geometry transformations
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
                    let width = rect.width() as i32;
                    let height = rect.height() as i32;

                    let callback = egui::PaintCallback {
                        rect,
                        callback: Arc::new(egui_glow::CallbackFn::new(move |_info, _painter| {
                            TLS_RENDER_CTX.with(|cell| {
                                if let Some(ref mut rc) = *cell.borrow_mut() {
                                    let _ = rc.render::<*mut std::os::raw::c_void>(0, width, height, false);
                                }
                            });
                        })),
                    };
                    ui.painter().add(callback);
                }

                // If empty state, show clean Fluent onboarding tip
                if self.current_file.is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new("\u{E72C}\nDrag & Drop Media to Play")
                            .font(egui::FontId::proportional(22.0))
                            .color(egui::Color32::from_white_alpha(140)));
                    });
                }
            });

        // Foreground Layer: Overlay Controls Ribbon
        if self.show_controls || !self.is_fullscreen {
            let panel_fill = egui::Color32::from_rgba_premultiplied(20, 20, 20, 215);
            
            egui::TopBottomPanel::bottom("fluent_ribbon")
                .frame(egui::Frame::default().fill(panel_fill).inner_margin(egui::Margin::symmetric(24.0, 14.0)))
                .show(ctx, |ui| {
                    
                    // --- ROW 1: Sleek Native Seekbar & Timers ---
                    ui.horizontal(|ui| {
                        // Current Position Label
                        ui.label(egui::RichText::new(Self::format_time(self.time_pos))
                            .font(egui::FontId::proportional(12.0))
                            .color(egui::Color32::from_rgb(200, 200, 200)));

                        // Custom Styled Interactive Seekbar Line
                        let slider_width = ui.available_width() - 55.0;
                        let mut progress = self.time_pos;
                        let max_duration = if self.duration > 0.0 { self.duration } else { 1.0 };
                        
                        ui.spacing_mut().slider_width = slider_width;
                        let seek_slider = ui.add(egui::Slider::new(&mut progress, 0.0..=max_duration)
                            .show_value(false)
                            .trailing_fill(true)); // Uses accent color for timeline tracking

                        if seek_slider.changed() {
                            let _ = self.mpv.lock().unwrap().set_property("time-pos", progress);
                        }

                        // Remaining / Duration Label
                        let time_string = if self.duration > 0.0 {
                            Self::format_time(self.duration)
                        } else {
                            "00:00".to_string()
                        };
                        ui.label(egui::RichText::new(time_string)
                            .font(egui::FontId::proportional(12.0))
                            .color(egui::Color32::from_rgb(140, 140, 140)));
                    });

                    ui.add_space(6.0);

                    // --- ROW 2: Media Transport Ribbon ---
                    ui.horizontal(|ui| {
                        // Play/Pause Control using standard Segoe Fluent Unicode Glyph strings
                        let play_icon = if self.is_playing { "\u{E103}" } else { "\u{E102}" };
                        let btn_play = ui.add(egui::Button::new(egui::RichText::new(play_icon).font(egui::FontId::proportional(16.0))));
                        if btn_play.clicked() {
                            self.is_playing = !self.is_playing;
                            let _ = self.mpv.lock().unwrap().set_property("pause", !self.is_playing);
                        }

                        // Stop Action Button
                        if ui.add(egui::Button::new(egui::RichText::new("\u{E15B}").font(egui::FontId::proportional(14.0)))).clicked() {
                            let _ = self.mpv.lock().unwrap().command("stop", &[]);
                            self.is_playing = false;
                            self.time_pos = 0.0;
                        }

                        ui.separator();

                        // Volume Control Sub-Ribbon
                        let vol_icon = if self.volume == 0 { "\u{E198}" } else if self.volume < 50 { "\u{E993}" } else { "\u{E15D}" };
                        ui.label(egui::RichText::new(vol_icon).font(egui::FontId::proportional(14.0)));
                        
                        let mut vol_float = self.volume as f32;
                        ui.spacing_mut().slider_width = 80.0;
                        if ui.add(egui::Slider::new(&mut vol_float, 0.0..=100.0).show_value(false)).changed() {
                            let _ = self.mpv.lock().unwrap().set_property("volume", vol_float as i64);
                        }

                        // Expand file string layout context to match left orientation layout boundaries
                        if !self.current_file.is_empty() {
                            ui.add_space(20.0);
                            let clean_name = Path::new(&self.current_file)
                                .file_name()
                                .map(|os| os.to_string_lossy().into_owned())
                                .unwrap_or_else(|| self.current_file.clone());
                            
                            ui.label(egui::RichText::new(clean_name)
                                .font(egui::FontId::proportional(13.0))
                                .color(egui::Color32::from_white_alpha(180)));
                        }

                        // Pull Remaining Control Icons to the Right hand side boundary
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let fs_icon = if self.is_fullscreen { "\u{E1D8}" } else { "\u{E1D9}" };
                            if ui.add(egui::Button::new(egui::RichText::new(fs_icon).font(egui::FontId::proportional(14.0)))).clicked() {
                                self.is_fullscreen = !self.is_fullscreen;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.is_fullscreen));
                            }
                        });
                    });
                });
        }

        ctx.request_repaint();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        // Configure native window canvas parameters for background blend pipelines
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 540.0])
            .with_title("Media Player")
            .with_decorations(true)
            .with_transparent(true), 
        ..Default::default()
    };

    eframe::run_native(
        "Media Player",
        options,
        Box::new(|cc| Box::new(FluentMediaPlayer::new(cc))),
    )
}
