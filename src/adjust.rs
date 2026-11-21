//! "Picture & Sound" panel and the equalizer panel.
//!
//! Everything here is plain settings plus drawing: the panels change an `Adjust`
//! value, and main.rs compares it with the previous one and sends the differences
//! to mpv (properties for brightness etc., FFmpeg filter chains for the rest).

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontFamily, FontId, Pos2, Rect, Sense, Stroke};
use crate::glass;
use std::path::Path;

// ── Settings ──────────────────────────────────────────────────────────────────

/// Equalizer bands (Hz) and their labels: the classic 10-band set
pub const EQ_FREQS: [u32; 10] = [60, 170, 310, 600, 1000, 3000, 6000, 12000, 14000, 16000];
const EQ_LABELS: [&str; 10] = ["60", "170", "310", "600", "1K", "3K", "6K", "12K", "14K", "16K"];
/// Band / preamp range in dB
const EQ_MAX: i32 = 12;

const EQ_PRESETS: [(&str, [i32; 10]); 9] = [
    ("Flat", [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    ("Bass boost", [6, 5, 4, 2, 0, 0, 0, 0, 0, 0]),
    ("Treble boost", [0, 0, 0, 0, 0, 2, 4, 5, 6, 6]),
    ("Bass & treble", [6, 4, 2, 0, -1, 0, 2, 4, 5, 5]),
    ("Vocal", [-2, -1, 0, 2, 4, 4, 3, 1, 0, -1]),
    ("Rock", [5, 3, -1, -3, -1, 2, 4, 5, 5, 5]),
    ("Pop", [-1, 2, 4, 4, 2, 0, -1, -1, -1, -1]),
    ("Classical", [0, 0, 0, 0, 0, 0, -4, -4, -4, -5]),
    ("Loudness", [6, 4, 0, -2, -1, 0, 0, 2, 4, 5]),
];

/// Volume normalizer: FFmpeg's "dynamic audio normalizer" (built into mpv). It looks
/// ~1 s ahead and smoothly raises quiet passages / lowers loud ones.
const NORMALIZER: &str = "lavfi=[dynaudnorm=f=150:g=15]";

#[derive(Clone, PartialEq, Debug)]
pub struct Adjust {
    // Picture: mpv's own controls, −100…100
    pub brightness: i32,
    pub contrast: i32,
    pub saturation: i32,
    pub gamma: i32,
    pub hue: i32,
    // Picture: FFmpeg filters
    /// −5 (cool) … +5 (warm), 600 K per step
    pub temperature: i32,
    /// −20 … +20, 0.05 of FFmpeg's vibrance strength per step
    pub vibrance: i32,
    /// 0 (off) … 5
    pub sharpen: i32,
    /// 0 (off), 1 low, 2 medium, 3 high
    pub denoise: i32,
    pub deband: bool,
    pub deinterlace: bool,
    pub smooth: bool,
    // Sound
    pub night: bool,
    pub dialogue: bool,
    /// Audio delay in ms (+ = sound later)
    pub delay_ms: i32,
    pub mono: bool,
    pub crossfeed: bool,
    pub widen: bool,
    pub keep_pitch: bool,
    // Equalizer (dB)
    pub eq_on: bool,
    pub preamp: i32,
    pub eq: [i32; 10],
}

impl Default for Adjust {
    fn default() -> Self {
        Self {
            brightness: 0,
            contrast: 0,
            saturation: 0,
            gamma: 0,
            hue: 0,
            temperature: 0,
            vibrance: 0,
            sharpen: 0,
            denoise: 0,
            deband: false,
            deinterlace: false,
            smooth: false,
            night: false,
            dialogue: false,
            delay_ms: 0,
            mono: false,
            crossfeed: false,
            widen: false,
            keep_pitch: true,
            eq_on: false,
            preamp: 0,
            eq: [0; 10],
        }
    }
}

impl Adjust {
    /// Read from `path` ("name=value" lines); anything missing keeps its default
    pub fn load(path: &Path) -> Self {
        let mut a = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else { return a };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let n = v.parse::<i32>().unwrap_or(0);
            let b = v == "1";
            match k {
                "brightness" => a.brightness = n.clamp(-100, 100),
                "contrast" => a.contrast = n.clamp(-100, 100),
                "saturation" => a.saturation = n.clamp(-100, 100),
                "gamma" => a.gamma = n.clamp(-100, 100),
                "hue" => a.hue = n.clamp(-100, 100),
                // (older files: −5…+5 in steps four times as big)
                "temperature" => a.temperature = n.clamp(-5, 5),
                // (briefly saved on a 4× finer scale)
                "temperature_fine" => a.temperature = ((n as f32) / 4.0).round() as i32,
                "vibrance" => a.vibrance = (n * 4).clamp(-20, 20),
                "vibrance_fine" => a.vibrance = n.clamp(-20, 20),
                "sharpen" => a.sharpen = n.clamp(0, 5),
                "denoise" => a.denoise = n.clamp(0, 3),
                "deband" => a.deband = b,
                "deinterlace" => a.deinterlace = b,
                "smooth" => a.smooth = b,
                "night" => a.night = b,
                "dialogue" => a.dialogue = b,
                "delay_ms" => a.delay_ms = n.clamp(-5000, 5000),
                "mono" => a.mono = b,
                "crossfeed" => a.crossfeed = b,
                "widen" => a.widen = b,
                "keep_pitch" => a.keep_pitch = b,
                "eq_on" => a.eq_on = b,
                "preamp" => a.preamp = n.clamp(-EQ_MAX, EQ_MAX),
                "eq" => {
                    for (i, g) in v.split(',').take(10).enumerate() {
                        a.eq[i] = g.trim().parse::<i32>().unwrap_or(0).clamp(-EQ_MAX, EQ_MAX);
                    }
                }
                _ => {}
            }
        }
        a
    }

    pub fn save(&self, path: &Path) {
        let b = |v: bool| if v { 1 } else { 0 };
        let eq: Vec<String> = self.eq.iter().map(|g| g.to_string()).collect();
        let text = format!(
            "brightness={}\ncontrast={}\nsaturation={}\ngamma={}\nhue={}\ntemperature={}\nvibrance_fine={}\n\
             sharpen={}\ndenoise={}\ndeband={}\ndeinterlace={}\nsmooth={}\nnight={}\ndialogue={}\n\
             delay_ms={}\nmono={}\ncrossfeed={}\nwiden={}\nkeep_pitch={}\neq_on={}\npreamp={}\neq={}\n",
            self.brightness, self.contrast, self.saturation, self.gamma, self.hue, self.temperature,
            self.vibrance, self.sharpen, self.denoise, b(self.deband), b(self.deinterlace), b(self.smooth),
            b(self.night), b(self.dialogue), self.delay_ms, b(self.mono), b(self.crossfeed), b(self.widen),
            b(self.keep_pitch), b(self.eq_on), self.preamp, eq.join(","),
        );
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }

    fn reset_picture(&mut self) {
        let d = Self::default();
        (self.brightness, self.contrast, self.saturation, self.gamma, self.hue) = (0, 0, 0, 0, 0);
        (self.temperature, self.vibrance, self.sharpen, self.denoise) = (0, 0, 0, 0);
        (self.deband, self.deinterlace, self.smooth) = (d.deband, d.deinterlace, d.smooth);
    }

    fn reset_sound(&mut self) {
        let d = Self::default();
        (self.night, self.dialogue, self.delay_ms) = (d.night, d.dialogue, d.delay_ms);
        (self.mono, self.crossfeed, self.widen, self.keep_pitch) = (d.mono, d.crossfeed, d.widen, d.keep_pitch);
    }

    /// Values for mpv's own colour controls (done on the GPU, as the video is
    /// turned into RGB)
    pub fn gpu_colour(&self) -> [(&'static str, i32); 5] {
        [
            ("brightness", self.brightness),
            ("contrast", self.contrast),
            ("saturation", self.saturation),
            ("gamma", self.gamma),
            ("hue", self.hue),
        ]
    }

    /// mpv's `vf` list (FFmpeg filters, run on the CPU), or None for no filters.
    /// Only Denoise and mirroring are left here: Color temperature, Vibrance and
    /// Sharpen run on the GPU (see `shader`). Done in FFmpeg they had every frame
    /// converted to RGB and processed on the CPU at full size, which a 4K 60 fps
    /// video couldn't keep up with. Denoise can't move (it compares each frame
    /// with the previous ones, which mpv's shaders can't see), so it's only used
    /// where `denoise_ok` says the video is small enough (1080p or less).
    pub fn vf_chain(&self, mirror: bool, denoise_ok: bool) -> Option<String> {
        let mut f: Vec<String> = Vec::new();
        if self.denoise > 0 && denoise_ok {
            f.push(format!("hqdn3d={}", [0, 2, 4, 7][self.denoise as usize]));
        }
        if mirror {
            f.push("hflip".to_owned());
        }
        (!f.is_empty()).then(|| format!("lavfi=[{}]", f.join(",")))
    }

    /// Short name for the current GPU picture shader's settings (used in its file
    /// name), or None when it isn't needed
    pub fn shader_key(&self) -> Option<String> {
        (self.temperature != 0 || self.vibrance != 0 || self.sharpen != 0)
            .then(|| format!("t{}-v{}-s{}", self.temperature, self.vibrance, self.sharpen))
    }

    /// An mpv user shader doing Color temperature, Vibrance and Sharpen on the GPU,
    /// with the settings written in as constants. It runs on the picture right
    /// after mpv's own colour conversion (where Brightness, Contrast, ... are
    /// applied), at the video's own size. The maths follows FFmpeg's
    /// colortemperature, vibrance and cas filters, which these used to be.
    pub fn shader(&self) -> Option<String> {
        self.shader_key()?;
        let mut body = String::new();
        if self.sharpen > 0 {
            // AMD's contrast-adaptive sharpening, as FFmpeg's cas filter does it
            let strength = self.sharpen as f32 * 0.2;
            let k = -(16.0 + (4.01 - 16.0) * strength);
            body += &format!(
                "    vec3 a = HOOKED_texOff(vec2(-1.0, -1.0)).rgb;\n\
                 \x20   vec3 b = HOOKED_texOff(vec2( 0.0, -1.0)).rgb;\n\
                 \x20   vec3 c = HOOKED_texOff(vec2( 1.0, -1.0)).rgb;\n\
                 \x20   vec3 d = HOOKED_texOff(vec2(-1.0,  0.0)).rgb;\n\
                 \x20   vec3 e = rgb;\n\
                 \x20   vec3 f = HOOKED_texOff(vec2( 1.0,  0.0)).rgb;\n\
                 \x20   vec3 g = HOOKED_texOff(vec2(-1.0,  1.0)).rgb;\n\
                 \x20   vec3 h = HOOKED_texOff(vec2( 0.0,  1.0)).rgb;\n\
                 \x20   vec3 i = HOOKED_texOff(vec2( 1.0,  1.0)).rgb;\n\
                 \x20   vec3 mn = min(min(min(d, e), min(f, b)), h);\n\
                 \x20   mn += min(min(min(mn, a), min(c, g)), i);\n\
                 \x20   vec3 mx = max(max(max(d, e), max(f, b)), h);\n\
                 \x20   mx += max(max(max(mx, a), max(c, g)), i);\n\
                 \x20   vec3 amp = sqrt(clamp(min(mn, 2.0 - mx) / max(mx, vec3(1e-5)), 0.0, 1.0));\n\
                 \x20   vec3 w = amp / {k:.6};\n\
                 \x20   rgb = clamp(((b + d + f + h) * w + e) / (1.0 + 4.0 * w), 0.0, 1.0);\n"
            );
        }
        if self.temperature != 0 {
            // 6500 K is "off"; lower is warmer
            let [r, g, b] = kelvin_rgb((6500 - self.temperature * 600) as f32);
            body += &format!("    rgb *= vec3({r:.6}, {g:.6}, {b:.6});\n");
        }
        if self.vibrance != 0 {
            // FFmpeg's vibrance (its default mode): a saturation boost that grows
            // with how colourful the pixel already is
            let v = self.vibrance as f32 * 0.05;
            let sign = if v >= 0.0 { 1.0 } else { -1.0 };
            body += &format!(
                "    float sat = max(rgb.r, max(rgb.g, rgb.b)) - min(rgb.r, min(rgb.g, rgb.b));\n\
                 \x20   float luma = dot(rgb, vec3(0.072186, 0.715158, 0.212656));\n\
                 \x20   rgb = mix(vec3(luma), rgb, 1.0 + {v:.6} * (1.0 + {sign:.1} * sat));\n"
            );
        }
        Some(format!(
            "//!HOOK MAIN\n//!BIND HOOKED\n//!DESC Rust Media Player picture\n\n\
             vec4 hook() {{\n    vec4 px = HOOKED_texOff(vec2(0.0));\n    vec3 rgb = px.rgb;\n{body}    return vec4(rgb, px.a);\n}}\n"
        ))
    }

    /// The equalizer's band gains as FFmpeg firequalizer wants them
    pub fn eq_entries(&self) -> String {
        EQ_FREQS
            .iter()
            .zip(self.eq.iter())
            .map(|(f, g)| format!("entry({f},{g})"))
            .collect::<Vec<_>>()
            .join(";")
    }

    /// mpv's `af` list, or None for no filters. With `with_gains` false the
    /// equalizer's gains are left out: comparing those versions tells whether only
    /// the gains changed (then they're sent live, without rebuilding the chain).
    pub fn af_chain(&self, normalize: bool, with_gains: bool) -> Option<String> {
        let mut v: Vec<String> = Vec::new();
        if self.eq_on {
            let (pre, entries) = if with_gains { (self.preamp, self.eq_entries()) } else { (0, String::new()) };
            v.push(format!("@eqpre:lavfi=[volume={pre}dB]"));
            v.push(format!("@eq:lavfi=[firequalizer=gain_entry='{entries}']"));
        }
        if self.dialogue {
            // (dialoguenhance takes stereo; it makes a centre channel for the voice)
            v.push("lavfi=[aformat=channel_layouts=stereo,dialoguenhance]".to_owned());
        }
        if self.mono {
            v.push("lavfi=[aformat=channel_layouts=mono]".to_owned());
        } else if self.widen || self.crossfeed {
            let mut g = vec!["aformat=channel_layouts=stereo"];
            if self.widen {
                g.push("extrastereo=m=1.6");
            }
            if self.crossfeed {
                g.push("crossfeed");
            }
            v.push(format!("lavfi=[{}]", g.join(",")));
        }
        if self.night {
            // threshold ≈ −21 dB, 4:1, quick attack, slow release, a little makeup gain
            v.push("lavfi=[acompressor=threshold=0.089:ratio=4:attack=5:release=250:makeup=2]".to_owned());
        }
        if normalize {
            v.push(NORMALIZER.to_owned());
        }
        (!v.is_empty()).then(|| v.join(","))
    }

    fn preset_name(&self) -> &'static str {
        EQ_PRESETS.iter().find(|(_, g)| *g == self.eq).map_or("Custom", |(n, _)| n)
    }
}

// ── Panel state ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Picture,
    Sound,
}

pub struct AdjustUi {
    pub open: bool,
    pub eq_open: bool,
    pub tab: Tab,
    /// Where the panels were drawn last (the mouse wheel there doesn't change the
    /// volume, and the mouse cursor isn't hidden over them)
    pub panel_rect: Rect,
    pub eq_rect: Rect,
    /// The row whose description is pending / showing, since when, and whether
    /// it's showing
    tip_row: Option<&'static str>,
    tip_since: std::time::Instant,
    tip_shown: bool,
    /// Denoise is on but not used for the current video (larger than 1080p);
    /// set by the app every frame, and mentioned in Denoise's description
    pub denoise_skipped: bool,
    /// The Online video settings panel (same place as Picture & Sound; one or the
    /// other) and its audio language list
    pub online_open: bool,
    pub online_rect: Rect,
    pub lang_open: bool,
    pub lang_rect: Rect,
    /// Scroll the language list to the chosen language (set when it opens)
    pub lang_scroll: bool,
}

impl AdjustUi {
    pub fn new() -> Self {
        Self {
            open: false,
            eq_open: false,
            tab: Tab::Picture,
            panel_rect: Rect::NOTHING,
            eq_rect: Rect::NOTHING,
            tip_row: None,
            tip_since: std::time::Instant::now(),
            tip_shown: false,
            denoise_skipped: false,
            online_open: false,
            online_rect: Rect::NOTHING,
            lang_open: false,
            lang_rect: Rect::NOTHING,
            lang_scroll: false,
        }
    }

    pub fn contains(&self, p: Pos2) -> bool {
        (self.open && self.panel_rect.contains(p))
            || (self.eq_open && self.eq_rect.contains(p))
            || (self.online_open && (self.online_rect.contains(p) || (self.lang_open && self.lang_rect.contains(p))))
    }
}

// ── Drawing helpers (the menu / shortcuts style) ──────────────────────────────

const BG: Color32 = Color32::from_rgb(44, 44, 44);
const TEXT: Color32 = Color32::from_rgb(240, 240, 240);
const DIM: Color32 = Color32::from_rgb(160, 160, 160);
const CAP: Color32 = Color32::from_rgb(64, 64, 66);
const CAP_EDGE: Color32 = Color32::from_rgb(52, 52, 54);
/// Highlight under the pointer, and while the mouse button is held on it
pub const HOVER: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
pub const PRESS: Color32 = Color32::from_rgba_premultiplied(58, 58, 58, 58);

/// The highlight for a clickable item: pressed (button held with the pointer still
/// on it), hovered, or none
/// Fires when the mouse button is released anywhere over the item, like Windows'
/// buttons (a plain "click" also needs the pointer to have stayed nearly still)
pub fn activated(resp: &egui::Response) -> bool {
    resp.clicked()
        || (resp.drag_stopped() && resp.ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| resp.rect.contains(p)))
}

/// Number of points for a round curve of radius `r` points, so each straight piece
/// is about 1.5 screen pixels long whatever the display scaling (egui's own circles
/// choose from the size in points, and small ones look like polygons above 100 %)
pub fn arc_points(r: f32, ppp: f32) -> usize {
    ((std::f32::consts::TAU * r * ppp / 1.5).ceil() as usize).clamp(24, 160)
}

/// A filled circle that stays smooth at any display scaling
pub fn disc(p: &egui::Painter, centre: Pos2, r: f32, color: Color32) {
    if r <= 0.0 || color == Color32::TRANSPARENT {
        return;
    }
    let n = arc_points(r, p.ctx().pixels_per_point());
    let pts = (0..n)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / n as f32;
            centre + r * egui::vec2(a.cos(), a.sin())
        })
        .collect();
    p.add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
}

pub fn state_fill(resp: &egui::Response) -> Option<Color32> {
    let inside = resp.ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| resp.rect.contains(p));
    if resp.is_pointer_button_down_on() && inside {
        Some(PRESS)
    } else if resp.hovered() {
        Some(HOVER)
    } else {
        None
    }
}
const LINE: Color32 = Color32::from_rgba_premultiplied(26, 26, 26, 26);
const NAME_SIZE: f32 = 13.5;
const VALUE_SIZE: f32 = 12.5;
const ROW_H: f32 = 30.0;
const HEAD_H: f32 = 44.0;

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

fn panel_frame(p: &egui::Painter, rect: Rect) {
    let r = 8.0;
    glass::shadow(p, rect, r, 1.0);
    glass::fill(p, rect, r, BG, 1.0);
    p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, glass::edge(Color32::from_rgba_premultiplied(30, 30, 30, 30))));
}

/// Title, "Reset" link and an X. Returns (reset clicked, close clicked).
fn header(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, title: &str, id: &str) -> (bool, bool) {
    header_with(ui, p, rect, title, id, true)
}

/// Title, optionally a "Reset" link, and an X. Returns (reset clicked, close clicked).
fn header_with(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, title: &str, id: &str, with_reset: bool) -> (bool, bool) {
    let cy = rect.top() + HEAD_H / 2.0;
    p.text(pos2(rect.left() + 16.0, cy), Align2::LEFT_CENTER, title, semibold(14.5), TEXT);

    let xr = Rect::from_center_size(pos2(rect.right() - 22.0, cy), vec2(28.0, 28.0));
    let x = ui.interact(xr, ui.id().with((id, "close")), Sense::click_and_drag());
    if let Some(c) = state_fill(&x) {
        p.rect_filled(xr, 4.0, c);
    }
    let c = xr.center();
    for (a, b) in [((-5.0, -5.0), (5.0, 5.0)), ((-5.0, 5.0), (5.0, -5.0))] {
        p.line_segment([c + vec2(a.0, a.1), c + vec2(b.0, b.1)], Stroke::new(1.2_f32, TEXT));
    }

    let mut reset_clicked = false;
    if with_reset {
        let font = FontId::proportional(12.5);
        let g = p.layout_no_wrap("Reset".to_owned(), font, DIM);
        let rr = Rect::from_center_size(pos2(xr.left() - 10.0 - g.size().x / 2.0, cy), g.size() + vec2(10.0, 8.0));
        let reset = ui.interact(rr, ui.id().with((id, "reset")), Sense::click_and_drag());
        if let Some(c) = state_fill(&reset) {
            p.rect_filled(rr, 4.0, c);
        }
        let col = if reset.hovered() { TEXT } else { DIM };
        p.galley(rr.center() - g.size() / 2.0, g, col);
        reset_clicked = activated(&reset);
    }

    p.line_segment(
        [pos2(rect.left() + 1.0, rect.top() + HEAD_H - 0.5), pos2(rect.right() - 1.0, rect.top() + HEAD_H - 0.5)],
        Stroke::new(1.0_f32, LINE),
    );
    (reset_clicked, activated(&x))
}

/// A key cap button ("−" / "+") with its label at `size`; returns true when clicked
fn cap_sized(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, label: &str, id: egui::Id, size: f32) -> bool {
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    let inside = resp.ctx.input(|i| i.pointer.latest_pos()).is_some_and(|p| rect.contains(p));
    let pressed = resp.is_pointer_button_down_on() && inside;
    let fill = if pressed {
        Color32::from_rgb(112, 112, 114)
    } else if resp.hovered() {
        Color32::from_rgb(82, 82, 84)
    } else {
        CAP
    };
    p.rect_filled(rect, 4.0, fill);
    // (the darker bottom edge stays when pressed, so the cap keeps its size; only
    // the label sinks a little)
    p.rect_filled(
        Rect::from_min_max(pos2(rect.left() + 1.0, rect.bottom() - 2.0), pos2(rect.right() - 1.0, rect.bottom())),
        2.0,
        CAP_EDGE,
    );
    let off = if pressed { 0.5 } else { -0.5 };
    p.text(rect.center() + vec2(0.0, off), Align2::CENTER_CENTER, label, semibold(size), TEXT);
    activated(&resp)
}

/// Windows 11-style on/off switch
fn switch(p: &egui::Painter, rect: Rect, on: bool, accent: Color32) {
    let r = rect.height() / 2.0;
    if on {
        p.rect_filled(rect, r, accent);
        disc(p, pos2(rect.right() - r, rect.center().y), r - 4.5, Color32::from_rgb(26, 26, 26));
    } else {
        p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, Color32::from_rgb(154, 154, 154)));
        disc(p, pos2(rect.left() + r, rect.center().y), r - 4.5, Color32::from_rgb(207, 207, 207));
    }
}

/// Description box beside a hovered row (after the pointer rests a moment)
fn description(ctx: &egui::Context, row: Rect, panel: Rect, text: &str) {
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("adjust_tip")));
    let g = p.layout(text.to_owned(), FontId::proportional(12.5), Color32::from_rgb(232, 232, 232), 240.0);
    let size = g.size() + vec2(20.0, 16.0);
    let screen = ctx.screen_rect();
    let mut x = panel.right() + 8.0;
    if x + size.x > screen.right() - 4.0 {
        x = (screen.right() - 4.0 - size.x).max(screen.left() + 4.0);
    }
    let y = (row.center().y - size.y / 2.0).clamp(screen.top() + 4.0, (screen.bottom() - 4.0 - size.y).max(screen.top() + 4.0));
    let r = Rect::from_min_size(pos2(x, y), size);
    glass::shadow(&p, r, 6.0, 0.8);
    glass::fill(&p, r, 6.0, Color32::from_rgb(43, 43, 43), 1.0);
    p.rect_stroke(r.shrink(0.5), 6.0, Stroke::new(1.0_f32, glass::edge(Color32::from_rgba_premultiplied(26, 26, 26, 26))));
    p.galley(r.min + vec2(10.0, 8.0), g, Color32::from_rgb(232, 232, 232));
}

fn signed(v: i32) -> String {
    if v > 0 { format!("+{v}") } else { v.to_string() }
}

// ── The "Picture & Sound" panel ───────────────────────────────────────────────

enum Row {
    /// name, description, value text, value changed from normal, (−, +) actions
    Step(&'static str, &'static str, String, bool, StepKey),
    /// name, description, on
    Switch(&'static str, &'static str, bool, SwitchKey),
    /// The equalizer button
    Equalizer(bool),
    Sep,
}

#[derive(Clone, Copy)]
enum StepKey {
    Brightness,
    Contrast,
    Saturation,
    Gamma,
    Hue,
    Temperature,
    Vibrance,
    Sharpen,
    Denoise,
    Delay,
}

#[derive(Clone, Copy)]
enum SwitchKey {
    Deband,
    Deinterlace,
    Smooth,
    Normalize,
    Night,
    Dialogue,
    Mono,
    Crossfeed,
    Widen,
    Pitch,
    Hq,
}

/// The controls with both ±5 and small ±1 buttons
fn has_fine(key: StepKey) -> bool {
    matches!(
        key,
        StepKey::Brightness | StepKey::Contrast | StepKey::Saturation | StepKey::Gamma | StepKey::Hue | StepKey::Vibrance
    )
}

/// One step up (dir 1) or down (−1); `fine` = ±1 instead of ±5 for the has_fine ones
fn step(a: &mut Adjust, key: StepKey, dir: i32, fine: bool) {
    let s = |v: &mut i32, by: i32, lo: i32, hi: i32| *v = (*v + by * dir).clamp(lo, hi);
    let big = if fine { 1 } else { 5 };
    match key {
        StepKey::Brightness => s(&mut a.brightness, big, -100, 100),
        StepKey::Contrast => s(&mut a.contrast, big, -100, 100),
        StepKey::Saturation => s(&mut a.saturation, big, -100, 100),
        StepKey::Gamma => s(&mut a.gamma, big, -100, 100),
        StepKey::Hue => s(&mut a.hue, big, -100, 100),
        StepKey::Temperature => s(&mut a.temperature, 1, -5, 5),
        StepKey::Vibrance => s(&mut a.vibrance, big, -20, 20),
        StepKey::Sharpen => s(&mut a.sharpen, 1, 0, 5),
        StepKey::Denoise => s(&mut a.denoise, 1, 0, 3),
        StepKey::Delay => s(&mut a.delay_ms, 50, -5000, 5000),
    }
}

fn flip(a: &mut Adjust, normalize: &mut bool, hq: &mut bool, key: SwitchKey) {
    let f = |v: &mut bool| *v = !*v;
    match key {
        SwitchKey::Deband => f(&mut a.deband),
        SwitchKey::Deinterlace => f(&mut a.deinterlace),
        SwitchKey::Smooth => f(&mut a.smooth),
        SwitchKey::Normalize => f(normalize),
        SwitchKey::Night => f(&mut a.night),
        SwitchKey::Dialogue => f(&mut a.dialogue),
        SwitchKey::Mono => f(&mut a.mono),
        SwitchKey::Crossfeed => f(&mut a.crossfeed),
        SwitchKey::Widen => f(&mut a.widen),
        SwitchKey::Pitch => f(&mut a.keep_pitch),
        SwitchKey::Hq => f(hq),
    }
}

fn rows(a: &Adjust, normalize: bool, hq: bool, tab: Tab, denoise_skipped: bool) -> Vec<Row> {
    let level = |v: i32| (signed(v), v != 0);
    match tab {
        Tab::Picture => {
            let (b, bc) = level(a.brightness);
            let (c, cc) = level(a.contrast);
            let (s, sc) = level(a.saturation);
            let (g, gc) = level(a.gamma);
            let (h, hc) = level(a.hue);
            let temp = match a.temperature {
                0 => "Off".to_owned(),
                t if t > 0 => format!("Warm {t}"),
                t => format!("Cool {}", -t),
            };
            let (v, vc) = level(a.vibrance);
            let sharpen = if a.sharpen == 0 { "Off".to_owned() } else { a.sharpen.to_string() };
            let denoise = ["Off", "Low", "Medium", "High"][a.denoise as usize].to_owned();
            vec![
                Row::Step("Brightness", "Lifts or lowers the whole picture. Too much washes out the blacks; for dark videos try Gamma first.", b, bc, StepKey::Brightness),
                Row::Step("Contrast", "The difference between dark and bright parts. Higher looks punchier, lower looks flatter.", c, cc, StepKey::Contrast),
                Row::Step("Saturation", "Colour strength. At \u{2212}100 the picture is black and white.", s, sc, StepKey::Saturation),
                Row::Step("Gamma", "Brightens or darkens the mid-tones without crushing blacks or whites. Often better than Brightness for dark videos.", g, gc, StepKey::Gamma),
                Row::Step("Hue", "Turns all colours around the colour wheel. Mostly for fixing a colour cast.", h, hc, StepKey::Hue),
                Row::Step("Vibrance", "A stronger colour boost than Saturation, strongest where colours are already rich.", v, vc, StepKey::Vibrance),
                Row::Step("Color temperature", "Warmer (more orange) or cooler (more blue) white balance.", temp, a.temperature != 0, StepKey::Temperature),
                Row::Step("Sharpen", "Crisper edges (AMD's contrast-adaptive sharpening). High levels can look harsh on grainy video.", sharpen, a.sharpen != 0, StepKey::Sharpen),
                Row::Step(
                    "Denoise",
                    if denoise_skipped {
                        "Smooths grain and compression noise. Only used on videos up to 1080p (it runs on the processor, which can't keep up with bigger ones), so it's off for this video."
                    } else {
                        "Smooths grain and compression noise. Only used on videos up to 1080p (it runs on the processor, which can't keep up with bigger ones)."
                    },
                    denoise,
                    a.denoise != 0,
                    StepKey::Denoise,
                ),
                Row::Sep,
                Row::Switch("Deband", "Smooths visible steps in gradients such as skies and dark scenes.", a.deband, SwitchKey::Deband),
                Row::Switch("Deinterlace", "Removes comb-like lines from old TV recordings and DVDs. Only acts on video that's actually interlaced.", a.deinterlace, SwitchKey::Deinterlace),
                Row::Switch("Smooth motion", "Matches films and other videos under 40 fps to your monitor's refresh rate, so panning shots judder less. (50 and 60 fps video doesn't need it, so it's left alone.)", a.smooth, SwitchKey::Smooth),
                Row::Switch("High-quality upscaling", "A sharper, cleaner scaler when the video is smaller than the window (e.g. 1080p on a 4K screen). Uses more graphics card time.", hq, SwitchKey::Hq),
            ]
        }
        Tab::Sound => {
            let delay = if a.delay_ms == 0 { "0 ms".to_owned() } else { format!("{} ms", signed(a.delay_ms)) };
            vec![
                Row::Equalizer(a.eq_on),
                Row::Sep,
                Row::Switch("Normalize volume", "Evens out loud and quiet passages, and songs from different albums.", normalize, SwitchKey::Normalize),
                Row::Switch("Night mode", "Tames sudden loud parts like explosions, so you can keep the volume low at night without losing dialogue.", a.night, SwitchKey::Night),
                Row::Switch("Dialogue boost", "Makes speech clearer by lifting the voice in the middle of the mix. Surround sound becomes stereo.", a.dialogue, SwitchKey::Dialogue),
                Row::Sep,
                Row::Step("Audio delay", "Fixes sound that's out of sync with the picture: + plays the sound later, \u{2212} earlier.", delay, a.delay_ms != 0, StepKey::Delay),
                Row::Sep,
                Row::Switch("Mono", "Mixes everything into one channel, e.g. for a single earbud.", a.mono, SwitchKey::Mono),
                Row::Switch("Headphone crossfeed", "Blends a little of each side into the other, so hard-panned old stereo recordings are less tiring on headphones.", a.crossfeed, SwitchKey::Crossfeed),
                Row::Switch("Stereo widening", "Makes the stereo sound wider.", a.widen, SwitchKey::Widen),
                Row::Switch("Keep pitch at other speeds", "Voices sound normal at 1.5\u{d7} or 0.5\u{d7}. Turn off for the chipmunk / slow-motion effect.", a.keep_pitch, SwitchKey::Pitch),
            ]
        }
    }
}

/// Top of both panels (below the menu button)
pub const PANEL_TOP: f32 = 44.0;
/// Space kept free above the bottom controls
const BOTTOM_GAP: f32 = 112.0;
const PANEL_W: f32 = 300.0;

pub fn draw_panel(ctx: &egui::Context, ui_state: &mut AdjustUi, a: &mut Adjust, normalize: &mut bool, hq: &mut bool, accent: Color32) {
    if !ui_state.open {
        ui_state.panel_rect = Rect::NOTHING;
        return;
    }
    let screen = ctx.screen_rect();
    let list = rows(a, *normalize, *hq, ui_state.tab, ui_state.denoise_skipped);
    let row_h = |r: &Row| if matches!(r, Row::Sep) { 9.0 } else { ROW_H };
    let content_h: f32 = list.iter().map(row_h).sum();
    const TABS_H: f32 = 42.0;
    let full_h = HEAD_H + TABS_H + content_h + 10.0;
    let avail = (screen.bottom() - BOTTOM_GAP) - (screen.top() + PANEL_TOP);
    let h = full_h.min(avail.max(HEAD_H + TABS_H + ROW_H * 2.0));
    let w = PANEL_W.min(screen.width() - 16.0);
    let rect = Rect::from_min_size(pos2(screen.left() + 8.0, screen.top() + PANEL_TOP), vec2(w, h));
    ui_state.panel_rect = rect;

    let id = egui::Id::new("adjust_panel");
    let mut hovered_desc: Option<(Rect, &'static str)> = None;
    let mut open_eq = false;
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        // The panel's own background swallows clicks / drags (they don't reach the video)
        ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter().clone();
        panel_frame(&p, rect);
        let (reset, close) = header(ui, &p, rect, "Picture & Sound", "adjust");
        if close {
            ui_state.open = false;
        }
        if reset {
            match ui_state.tab {
                Tab::Picture => {
                    a.reset_picture();
                    *hq = false;
                }
                Tab::Sound => {
                    a.reset_sound();
                    *normalize = false;
                }
            }
        }

        // Tabs
        let tabs = Rect::from_min_size(pos2(rect.left() + 12.0, rect.top() + HEAD_H + 8.0), vec2(w - 24.0, 28.0));
        for (i, (tab, label)) in [(Tab::Picture, "Picture"), (Tab::Sound, "Sound")].into_iter().enumerate() {
            let half = (tabs.width() - 4.0) / 2.0;
            let tr = Rect::from_min_size(pos2(tabs.left() + i as f32 * (half + 4.0), tabs.top()), vec2(half, tabs.height()));
            let resp = ui.interact(tr, ui.id().with(("tab", i)), Sense::click_and_drag());
            let on = ui_state.tab == tab;
            if on {
                // The selected tab: an accent shape (bottom half only, so none of
                // it peeks out at the top corners) with the tab drawn over it 2 px
                // shorter, leaving an accent edge along the bottom that follows the
                // rounded corners up the sides. The top corners keep the same
                // radius as the other tab.
                let lower = Rect::from_min_max(pos2(tr.left() - 1.0, tr.center().y), pos2(tr.right() + 1.0, tr.bottom() + 1.0));
                p.with_clip_rect(lower).rect_filled(tr, 5.0, accent);
                p.rect_filled(Rect::from_min_max(tr.min, pos2(tr.right(), tr.bottom() - 2.0)), 5.0, Color32::from_rgb(68, 68, 68));
            } else {
                let fill = state_fill(&resp).unwrap_or(Color32::from_rgba_premultiplied(8, 8, 8, 8));
                p.rect_filled(tr, 5.0, fill);
            }
            let (font, col) = if on { (semibold(NAME_SIZE), TEXT) } else { (FontId::proportional(NAME_SIZE), Color32::from_rgb(200, 200, 200)) };
            p.text(tr.center(), Align2::CENTER_CENTER, label, font, col);
            if activated(&resp) {
                ui_state.tab = tab;
            }
        }

        // Rows (scroll when the window is too short)
        let list_rect = Rect::from_min_max(pos2(rect.left() + 6.0, rect.top() + HEAD_H + TABS_H), pos2(rect.right() - 6.0, rect.bottom() - 6.0));
        let mut child = ui.child_ui(list_rect, egui::Layout::top_down(egui::Align::Min));
        egui::ScrollArea::vertical().max_height(list_rect.height()).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let rw = list_rect.width() - if full_h > h { 8.0 } else { 0.0 };
            for row in &list {
                let (r, _) = ui.allocate_exact_size(vec2(rw, row_h(row)), Sense::hover());
                let p = ui.painter().clone();
                match row {
                    Row::Sep => {
                        p.line_segment([pos2(r.left() + 4.0, r.center().y), pos2(r.right() - 4.0, r.center().y)], Stroke::new(1.0_f32, LINE));
                    }
                    Row::Step(name, desc, value, changed, key) => {
                        if ui.rect_contains_pointer(r) {
                            p.rect_filled(r, 4.0, HOVER);
                            hovered_desc = Some((r, desc));
                        }
                        p.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, *name, FontId::proportional(NAME_SIZE), TEXT);
                        // Outer caps: the normal step (±5 for the −100…100 ones), a size
                        // bigger; inner tiny caps on those: ±1
                        let plus = Rect::from_center_size(pos2(r.right() - 19.0, r.center().y), vec2(26.0, 22.0));
                        let minus = plus.translate(vec2(-112.0, 0.0));
                        let col = if *changed { accent } else { Color32::from_rgb(189, 189, 189) };
                        // (long values like "Cool 20" get a smaller size to fit between the small buttons)
                        let size = if value.chars().count() > 6 { 11.0 } else { VALUE_SIZE };
                        p.text(pos2((minus.right() + plus.left()) / 2.0, r.center().y), Align2::CENTER_CENTER, value, FontId::proportional(size), col);
                        if cap_sized(ui, &p, minus, "\u{2212}", ui.id().with((*name, "minus")), 14.5) {
                            step(a, *key, -1, false);
                        }
                        if cap_sized(ui, &p, plus, "+", ui.id().with((*name, "plus")), 14.5) {
                            step(a, *key, 1, false);
                        }
                        if has_fine(*key) {
                            let fplus = Rect::from_center_size(pos2(plus.left() - 12.0, r.center().y), vec2(15.0, 15.0));
                            let fminus = Rect::from_center_size(pos2(minus.right() + 12.0, r.center().y), vec2(15.0, 15.0));
                            if cap_sized(ui, &p, fminus, "\u{2212}", ui.id().with((*name, "minus1")), 10.0) {
                                step(a, *key, -1, true);
                            }
                            if cap_sized(ui, &p, fplus, "+", ui.id().with((*name, "plus1")), 10.0) {
                                step(a, *key, 1, true);
                            }
                        }
                        // The wheel over a row steps it too (the normal step)
                        if ui.rect_contains_pointer(r) {
                            let wheel = ui.input(|i| i.raw_scroll_delta.y);
                            if wheel > 0.0 {
                                step(a, *key, 1, false);
                            } else if wheel < 0.0 {
                                step(a, *key, -1, false);
                            }
                        }
                    }
                    Row::Switch(name, desc, on, key) => {
                        let resp = ui.interact(r, ui.id().with((*name, "switch")), Sense::click_and_drag());
                        if let Some(c) = state_fill(&resp) {
                            p.rect_filled(r, 4.0, c);
                        }
                        if resp.hovered() {
                            hovered_desc = Some((r, desc));
                        }
                        p.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, *name, FontId::proportional(NAME_SIZE), TEXT);
                        switch(&p, Rect::from_center_size(pos2(r.right() - 28.0, r.center().y), vec2(38.0, 19.0)), *on, accent);
                        if activated(&resp) {
                            flip(a, normalize, hq, *key);
                        }
                    }
                    Row::Equalizer(on) => {
                        let resp = ui.interact(r, ui.id().with("eq_button"), Sense::click_and_drag());
                        if let Some(c) = state_fill(&resp) {
                            p.rect_filled(r, 4.0, c);
                        }
                        if resp.hovered() {
                            hovered_desc = Some((r, "Adjust bass, mids and treble with a 10-band equalizer."));
                        }
                        p.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, "Equalizer\u{2026}", FontId::proportional(NAME_SIZE), TEXT);
                        let state = if *on { "On" } else { "Off" };
                        let col = if *on { accent } else { DIM };
                        p.text(pos2(r.right() - 26.0, r.center().y), Align2::RIGHT_CENTER, state, FontId::proportional(VALUE_SIZE), col);
                        // chevron
                        let c = pos2(r.right() - 14.0, r.center().y);
                        p.line_segment([c + vec2(-2.5, -4.5), c + vec2(2.0, 0.0)], Stroke::new(1.3_f32, TEXT));
                        p.line_segment([c + vec2(2.0, 0.0), c + vec2(-2.5, 4.5)], Stroke::new(1.3_f32, TEXT));
                        if activated(&resp) {
                            open_eq = true;
                        }
                    }
                }
            }
        });
    });
    if open_eq {
        ui_state.eq_open = !ui_state.eq_open;
    }
    show_tip(ctx, ui_state, hovered_desc, rect);
}

/// A row's description appears after the pointer has been on that row for a
/// moment, then stays while the pointer is anywhere on the row (moving or not).
/// Moving to another row while one is showing switches straight to the new one.
fn show_tip(ctx: &egui::Context, ui_state: &mut AdjustUi, hovered_desc: Option<(Rect, &'static str)>, rect: Rect) {
    const TIP_DELAY: f32 = 0.35;
    match hovered_desc {
        Some((row, desc)) => {
            if ui_state.tip_row != Some(desc) {
                let now = std::time::Instant::now();
                ui_state.tip_since = if ui_state.tip_shown {
                    now.checked_sub(std::time::Duration::from_secs_f32(TIP_DELAY)).unwrap_or(now)
                } else {
                    now
                };
                ui_state.tip_row = Some(desc);
            }
            ui_state.tip_shown = ui_state.tip_since.elapsed().as_secs_f32() >= TIP_DELAY;
            if ui_state.tip_shown {
                description(ctx, row, rect, desc);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(60));
            }
        }
        None => {
            ui_state.tip_row = None;
            ui_state.tip_shown = false;
        }
    }
}

// ── The equalizer panel ───────────────────────────────────────────────────────

pub fn draw_eq(ctx: &egui::Context, ui_state: &mut AdjustUi, a: &mut Adjust, accent: Color32) {
    if !ui_state.eq_open {
        ui_state.eq_rect = Rect::NOTHING;
        return;
    }
    const COL_W: f32 = 24.0;
    const PAD: f32 = 16.0;
    const GAP: f32 = 14.0;
    const TRACK_H: f32 = 124.0;
    let w = PAD * 2.0 + COL_W * 11.0 + GAP;
    let h = HEAD_H + 40.0 + 20.0 + TRACK_H + 26.0 + 8.0;
    let screen = ctx.screen_rect();
    // Beside the Picture & sound panel when there's room, otherwise on top of it
    let beside = ui_state.panel_rect.is_positive() && ui_state.panel_rect.right() + 8.0 + w <= screen.right() - 8.0;
    let left = if beside { ui_state.panel_rect.right() + 8.0 } else { screen.left() + 8.0 };
    let rect = Rect::from_min_size(pos2(left, screen.top() + PANEL_TOP), vec2(w, h));
    ui_state.eq_rect = rect;

    let id = egui::Id::new("eq_panel");
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter().clone();
        panel_frame(&p, rect);
        let (reset, close) = header(ui, &p, rect, "Equalizer", "eq");
        if close {
            ui_state.eq_open = false;
        }
        if reset {
            a.eq = [0; 10];
            a.preamp = 0;
        }

        // On / off switch and the preset picker (‹ name ›)
        let cy = rect.top() + HEAD_H + 20.0;
        let sw_row = Rect::from_min_size(pos2(rect.left() + 8.0, cy - 14.0), vec2(80.0, 28.0));
        let sw = ui.interact(sw_row, ui.id().with("eq_on"), Sense::click_and_drag());
        if let Some(c) = state_fill(&sw) {
            p.rect_filled(sw_row, 4.0, c);
        }
        switch(&p, Rect::from_min_size(pos2(sw_row.left() + 8.0, cy - 9.5), vec2(38.0, 19.0)), a.eq_on, accent);
        p.text(pos2(sw_row.left() + 54.0, cy), Align2::LEFT_CENTER, if a.eq_on { "On" } else { "Off" }, FontId::proportional(NAME_SIZE), TEXT);
        if activated(&sw) {
            a.eq_on = !a.eq_on;
        }
        let next = Rect::from_center_size(pos2(rect.right() - PAD - 13.0, cy), vec2(26.0, 22.0));
        let prev = next.translate(vec2(-132.0, 0.0));
        p.text(pos2((prev.right() + next.left()) / 2.0, cy), Align2::CENTER_CENTER, a.preset_name(), FontId::proportional(VALUE_SIZE), TEXT);
        let cur = EQ_PRESETS.iter().position(|(_, g)| *g == a.eq);
        let n = EQ_PRESETS.len();
        if cap_sized(ui, &p, prev, "\u{2039}", ui.id().with("eq_prev"), 15.5) {
            a.eq = EQ_PRESETS[cur.map_or(0, |i| (i + n - 1) % n)].1;
        }
        if cap_sized(ui, &p, next, "\u{203a}", ui.id().with("eq_next"), 15.5) {
            a.eq = EQ_PRESETS[cur.map_or(0, |i| (i + 1) % n)].1;
        }

        // Sliders: preamp, then the 10 bands
        let top = rect.top() + HEAD_H + 40.0;
        let track_top = top + 20.0;
        let ppp = ctx.pixels_per_point();
        let snap = |v: f32| (v * ppp).round() / ppp;
        let dim = if a.eq_on { 1.0 } else { 0.45 };
        for col in 0..11 {
            let x = rect.left() + PAD + COL_W * (col as f32 + 0.5) + if col > 0 { GAP } else { 0.0 };
            let value = if col == 0 { a.preamp } else { a.eq[col - 1] };
            let label = if col == 0 { "Pre" } else { EQ_LABELS[col - 1] };
            let hit = Rect::from_min_max(pos2(x - COL_W / 2.0, track_top - 8.0), pos2(x + COL_W / 2.0, track_top + TRACK_H + 8.0));
            let resp = ui.interact(hit, ui.id().with(("band", col)), Sense::click_and_drag());

            // Track, the 0 dB mark, and the fill from 0 dB to the knob
            let y_of = |v: i32| track_top + TRACK_H * (EQ_MAX - v) as f32 / (2 * EQ_MAX) as f32;
            let tx = snap(x);
            p.rect_filled(Rect::from_min_max(pos2(tx - 1.5, track_top), pos2(tx + 1.5, track_top + TRACK_H)), 1.5, Color32::from_rgb(80, 80, 82));
            p.line_segment([pos2(tx - 5.0, snap(y_of(0))), pos2(tx + 5.0, snap(y_of(0)))], Stroke::new(1.0_f32, Color32::from_rgb(110, 110, 112)));
            let (y0, y1) = (y_of(0).min(y_of(value)), y_of(0).max(y_of(value)));
            if value != 0 {
                p.rect_filled(Rect::from_min_max(pos2(tx - 1.5, y0), pos2(tx + 1.5, y1)), 1.5, accent.gamma_multiply(dim));
            }
            let hot = resp.hovered() || resp.dragged();
            let ky = snap(y_of(value));
            disc(&p, pos2(tx, ky + 1.0), if hot { 7.3 } else { 6.6 }, Color32::from_black_alpha(80));
            disc(&p, pos2(tx, ky), if hot { 7.0 } else { 6.2 }, Color32::from_rgb(186, 186, 188));
            disc(&p, pos2(tx, ky), if hot { 4.2 } else { 3.6 }, accent.gamma_multiply(dim.max(0.7)));

            p.text(pos2(x, top + 8.0), Align2::CENTER_CENTER, signed(value), FontId::proportional(11.5), if value != 0 { TEXT } else { DIM });
            p.text(pos2(x, track_top + TRACK_H + 16.0), Align2::CENTER_CENTER, label, FontId::proportional(11.5), DIM);

            // Drag / click to set, double-click for 0 dB, wheel for ±1
            let mut v = value;
            if resp.double_clicked() {
                v = 0;
            } else if resp.dragged() || resp.is_pointer_button_down_on() {
                if let Some(pp) = resp.interact_pointer_pos() {
                    let f = ((pp.y - track_top) / TRACK_H).clamp(0.0, 1.0);
                    v = (EQ_MAX as f32 - f * (2 * EQ_MAX) as f32).round() as i32;
                }
            }
            if resp.hovered() {
                let wheel = ui.input(|i| i.raw_scroll_delta.y);
                if wheel > 0.0 {
                    v += 1;
                } else if wheel < 0.0 {
                    v -= 1;
                }
            }
            let v = v.clamp(-EQ_MAX, EQ_MAX);
            if col == 0 {
                a.preamp = v;
            } else {
                a.eq[col - 1] = v;
            }
        }
        // Divider between the preamp and the bands
        let dx = rect.left() + PAD + COL_W + GAP / 2.0;
        p.line_segment([pos2(dx, track_top - 4.0), pos2(dx, track_top + TRACK_H + 4.0)], Stroke::new(1.0_f32, LINE));
    });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
}

/// White balance multipliers for a colour temperature in kelvin, as FFmpeg's
/// colortemperature filter computes them (Tanner Helland's approximation)
fn kelvin_rgb(k: f32) -> [f32; 3] {
    let kelvin = k / 100.0;
    let (r, g) = if kelvin <= 66.0 {
        (1.0, 0.390_081_58 * kelvin.ln() - 0.631_841_4)
    } else {
        let t = (kelvin - 60.0).max(0.0);
        (1.292_936_2 * t.powf(-0.133_204_76), 1.129_890_9 * t.powf(-0.075_514_85))
    };
    let b = if kelvin >= 66.0 {
        1.0
    } else if kelvin <= 19.0 {
        0.0
    } else {
        0.543_206_8 * (kelvin - 10.0).ln() - 1.196_254_1
    };
    [r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]
}

// ── The "Online video settings" panel ─────────────────────────────────────────

/// 2–3 choices side by side, ending at `right`; the chosen one raised, with an
/// accent edge along its bottom like the selected tab. Returns the one clicked.
#[allow(clippy::too_many_arguments)]
fn segmented(ui: &mut egui::Ui, p: &egui::Painter, right: f32, cy: f32, labels: &[&str], chosen: usize, id: &str, accent: Color32) -> Option<usize> {
    const PAD: f32 = 9.0;
    let widths: Vec<f32> = labels
        .iter()
        .map(|l| p.layout_no_wrap((*l).to_owned(), semibold(12.0), TEXT).size().x + PAD * 2.0)
        .collect();
    let total = widths.iter().sum::<f32>() + 2.0 * (labels.len() as f32 - 1.0) + 4.0;
    let outer = Rect::from_min_size(pos2(right - total, cy - 12.0), vec2(total, 24.0));
    p.rect_filled(outer, 5.0, Color32::from_rgb(52, 52, 54));
    let mut x = outer.left() + 2.0;
    let mut clicked = None;
    for (i, (label, w)) in labels.iter().zip(&widths).enumerate() {
        let r = Rect::from_min_size(pos2(x, outer.top() + 2.0), vec2(*w, 20.0));
        let resp = ui.interact(r, ui.id().with((id, i)), Sense::click_and_drag());
        if i == chosen {
            let lower = Rect::from_min_max(pos2(r.left() - 1.0, r.center().y), pos2(r.right() + 1.0, r.bottom() + 1.0));
            p.with_clip_rect(lower).rect_filled(r, 4.0, accent);
            p.rect_filled(Rect::from_min_max(r.min, pos2(r.right(), r.bottom() - 2.0)), 4.0, Color32::from_rgb(84, 84, 86));
            p.text(r.center() - vec2(0.0, 1.0), Align2::CENTER_CENTER, *label, semibold(12.0), TEXT);
        } else {
            if let Some(c) = state_fill(&resp) {
                p.rect_filled(r, 4.0, c);
            }
            p.text(r.center(), Align2::CENTER_CENTER, *label, FontId::proportional(12.0), Color32::from_rgb(200, 200, 200));
        }
        if activated(&resp) {
            clicked = Some(i);
        }
        x += w + 2.0;
    }
    clicked
}

/// The Online video settings panel: which formats to ask yt-dlp for when a link
/// is opened (top left, where Picture & Sound goes; one or the other)
pub fn draw_online(ctx: &egui::Context, ui_state: &mut AdjustUi, o: &mut crate::online::Online, accent: Color32) {
    use crate::online::{AudioCodec, Online, VideoCodec, LANGUAGES};
    if !ui_state.online_open {
        ui_state.online_rect = Rect::NOTHING;
        ui_state.lang_open = false;
        return;
    }
    const NOTE_H: f32 = 34.0;
    let screen = ctx.screen_rect();
    let w = PANEL_W.min(screen.width() - 16.0);
    let h = HEAD_H + 6.0 + ROW_H * 6.0 + 9.0 + NOTE_H;
    let rect = Rect::from_min_size(pos2(screen.left() + 8.0, screen.top() + PANEL_TOP), vec2(w, h));
    ui_state.online_rect = rect;

    let mut hovered_desc: Option<(Rect, &'static str)> = None;
    let mut lang_row = Rect::NOTHING;
    let id = egui::Id::new("online_panel");
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter().clone();
        panel_frame(&p, rect);
        let (reset, close) = header(ui, &p, rect, "Online video settings", "online");
        if close {
            ui_state.online_open = false;
        }
        if reset {
            *o = Online::default();
        }
        let mut top = rect.top() + HEAD_H + 6.0;
        let mut next_row = |hgt: f32| {
            let r = Rect::from_min_size(pos2(rect.left() + 6.0, top), vec2(w - 12.0, hgt));
            top += hgt;
            r
        };
        let name = |p: &egui::Painter, r: Rect, text: &str| {
            p.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(NAME_SIZE), TEXT);
        };
        let hover_row = |ui: &egui::Ui, p: &egui::Painter, r: Rect| {
            let over = ui.rect_contains_pointer(r);
            if over {
                p.rect_filled(r, 4.0, HOVER);
            }
            over
        };

        // Video format
        let r = next_row(ROW_H);
        if hover_row(ui, &p, r) {
            hovered_desc = Some((r, "Which kind of video to ask for when a site offers several. VP9 and H.264 are decoded by the graphics card on most PCs; AV1 only on newer ones, and it's heavy for older PCs."));
        }
        name(&p, r, "Video format");
        let chosen = match o.codec {
            VideoCodec::H264 => 0,
            VideoCodec::Vp9 => 1,
            VideoCodec::Av1 => 2,
        };
        if let Some(i) = segmented(ui, &p, r.right() - 6.0, r.center().y, &["H.264", "VP9", "AV1"], chosen, "codec", accent) {
            o.codec = [VideoCodec::H264, VideoCodec::Vp9, VideoCodec::Av1][i];
        }

        // Max quality
        let r = next_row(ROW_H);
        if hover_row(ui, &p, r) {
            hovered_desc = Some((r, "The largest picture to play. Smaller uses less internet and computer power. \"Audio only\" plays just the sound."));
        }
        name(&p, r, "Max quality");
        let plus = Rect::from_center_size(pos2(r.right() - 19.0, r.center().y), vec2(26.0, 22.0));
        let minus = plus.translate(vec2(-112.0, 0.0));
        let col = if o.max_height != Online::default().max_height { accent } else { Color32::from_rgb(189, 189, 189) };
        p.text(pos2((minus.right() + plus.left()) / 2.0, r.center().y), Align2::CENTER_CENTER, o.quality_label(), FontId::proportional(VALUE_SIZE), col);
        if cap_sized(ui, &p, minus, "\u{2212}", ui.id().with("quality_minus"), 14.5) {
            o.step_quality(-1);
        }
        if cap_sized(ui, &p, plus, "+", ui.id().with("quality_plus"), 14.5) {
            o.step_quality(1);
        }

        // Switches
        for (label, desc, key) in [
            ("50/60 fps", "Prefer the smoother 50 or 60 fps version of a video when there is one.", 0),
            ("HDR", "Prefer HDR versions. Only worth it on an HDR display; on a normal one HDR video tends to look washed out.", 1),
        ] {
            let r = next_row(ROW_H);
            let resp = ui.interact(r, ui.id().with(("online_switch", key)), Sense::click_and_drag());
            if let Some(c) = state_fill(&resp) {
                p.rect_filled(r, 4.0, c);
            }
            if resp.hovered() {
                hovered_desc = Some((r, desc));
            }
            name(&p, r, label);
            let on = if key == 0 { o.hfr } else { o.hdr };
            switch(&p, Rect::from_center_size(pos2(r.right() - 28.0, r.center().y), vec2(38.0, 19.0)), on, accent);
            if activated(&resp) {
                if key == 0 {
                    o.hfr = !o.hfr;
                } else {
                    o.hdr = !o.hdr;
                }
            }
        }

        let r = next_row(9.0);
        p.line_segment([pos2(r.left() + 4.0, r.center().y), pos2(r.right() - 4.0, r.center().y)], Stroke::new(1.0_f32, LINE));

        // Audio format
        let r = next_row(ROW_H);
        if hover_row(ui, &p, r) {
            hovered_desc = Some((r, "Which kind of sound to ask for. On YouTube, Opus is usually the better quality."));
        }
        name(&p, r, "Audio format");
        let chosen = if o.audio == AudioCodec::Aac { 0 } else { 1 };
        if let Some(i) = segmented(ui, &p, r.right() - 6.0, r.center().y, &["AAC", "Opus"], chosen, "audio", accent) {
            o.audio = if i == 0 { AudioCodec::Aac } else { AudioCodec::Opus };
        }

        // Audio language (opens a list beside the panel)
        let r = next_row(ROW_H);
        lang_row = r;
        let resp = ui.interact(r, ui.id().with("lang_row"), Sense::click_and_drag());
        if ui_state.lang_open {
            p.rect_filled(r, 4.0, HOVER);
        } else if let Some(c) = state_fill(&resp) {
            p.rect_filled(r, 4.0, c);
        }
        if resp.hovered() && !ui_state.lang_open {
            hovered_desc = Some((r, "For videos with sound in several languages (dubbed versions). Default plays the video's own."));
        }
        name(&p, r, "Audio language");
        let c = pos2(r.right() - 14.0, r.center().y);
        p.text(pos2(r.right() - 26.0, r.center().y), Align2::RIGHT_CENTER, o.language_name(), FontId::proportional(VALUE_SIZE), Color32::from_rgb(220, 220, 220));
        p.line_segment([c + vec2(-2.5, -4.5), c + vec2(2.0, 0.0)], Stroke::new(1.3_f32, TEXT));
        p.line_segment([c + vec2(2.0, 0.0), c + vec2(-2.5, 4.5)], Stroke::new(1.3_f32, TEXT));
        if activated(&resp) {
            ui_state.lang_open = !ui_state.lang_open;
            ui_state.lang_scroll = ui_state.lang_open;
        }

        // Note
        let r = next_row(NOTE_H);
        p.text(
            pos2(r.left() + 10.0, r.center().y),
            Align2::LEFT_CENTER,
            "yt-dlp updates itself if a link fails to open.",
            FontId::proportional(11.5),
            Color32::from_rgb(150, 150, 150),
        );
    });

    // The language list, beside the panel next to its row
    if ui_state.lang_open {
        const ITEM_H: f32 = 24.0;
        // Whole rows only (at most 12), so the list never ends on half a name
        let rows = (((screen.height() - 22.0) / ITEM_H).floor() as usize).clamp(4, 12).min(LANGUAGES.len());
        let list_h = rows as f32 * ITEM_H + 6.0;
        let lw = 190.0;
        let x = (rect.right() + 6.0).min(screen.right() - lw - 8.0);
        let y = (lang_row.top() - 3.0).clamp(screen.top() + 8.0, (screen.bottom() - 8.0 - list_h).max(screen.top() + 8.0));
        let lr = Rect::from_min_size(pos2(x, y), vec2(lw, list_h));
        ui_state.lang_rect = lr;
        // A press outside the list (other than on its row, which toggles it) closes it
        let outside = ctx.input(|i| {
            i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|q| !lr.contains(q) && !lang_row.contains(q))
        });
        if outside {
            ui_state.lang_open = false;
        } else {
            let lid = egui::Id::new("online_lang_list");
            egui::Area::new(lid).order(egui::Order::Foreground).fixed_pos(lr.min).show(ctx, |ui| {
                ui.allocate_rect(lr, Sense::click_and_drag());
                let p = ui.painter().clone();
                panel_frame(&p, lr);
                let inner = lr.shrink(3.0);
                let mut child = ui.child_ui(inner, egui::Layout::top_down(egui::Align::Min));
                egui::ScrollArea::vertical().max_height(inner.height()).show(&mut child, |ui| {
                    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                    for (code, lang) in LANGUAGES {
                        let (r, _) = ui.allocate_exact_size(vec2(inner.width() - 8.0, ITEM_H), Sense::hover());
                        let resp = ui.interact(r, ui.id().with(("lang", code)), Sense::click_and_drag());
                        let p = ui.painter().clone();
                        if let Some(c) = state_fill(&resp) {
                            p.rect_filled(r, 4.0, c);
                        }
                        if o.language == code {
                            disc(&p, pos2(r.left() + 12.0, r.center().y), 2.5, TEXT);
                            if ui_state.lang_scroll {
                                ui.scroll_to_rect(r, Some(egui::Align::Center));
                            }
                        }
                        p.text(pos2(r.left() + 24.0, r.center().y), Align2::LEFT_CENTER, lang, FontId::proportional(13.0), TEXT);
                        if activated(&resp) {
                            o.language = code;
                            ui_state.lang_open = false;
                        }
                    }
                    ui_state.lang_scroll = false;
                });
            });
            ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, lid));
        }
    } else {
        ui_state.lang_rect = Rect::NOTHING;
    }
    show_tip(ctx, ui_state, hovered_desc, rect);
}

/// A button filled with the accent colour, white label (the main action of a
/// box: Open, Show in folder). Dimmed and inert when not `enabled`.
#[allow(clippy::too_many_arguments)]
fn accent_button(ui: &mut egui::Ui, p: &egui::Painter, r: Rect, label: &str, id: egui::Id, enabled: bool, accent: Color32) -> bool {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let inside = ui.ctx().input(|i| i.pointer.latest_pos()).is_some_and(|q| r.contains(q));
    let fill = if !enabled {
        accent.gamma_multiply(0.35)
    } else if resp.is_pointer_button_down_on() && inside {
        accent.gamma_multiply(0.8)
    } else if resp.hovered() {
        accent.gamma_multiply(0.9)
    } else {
        accent
    };
    p.rect_filled(r, 4.0, fill);
    let text = if enabled { Color32::WHITE } else { Color32::from_white_alpha(110) };
    p.text(r.center(), Align2::CENTER_CENTER, label, semibold(13.0), text);
    enabled && activated(&resp)
}

// ── The Open URL box ──────────────────────────────────────────────────────────

pub enum UrlBoxAction {
    None,
    Open(String),
    Cancel,
}

/// The Open URL box in the middle of the window: a text field (filled in from the
/// clipboard when it holds a link), Cancel and Open. Enter opens.
pub fn draw_url_box(ctx: &egui::Context, b: &mut crate::online::UrlBox, accent: Color32) -> UrlBoxAction {
    let screen = ctx.screen_rect();
    let w = 440.0_f32.min(screen.width() - 24.0);
    let h = HEAD_H + 134.0;
    let rect = Rect::from_center_size(screen.center(), vec2(w, h));
    let mut action = UrlBoxAction::None;
    let id = egui::Id::new("url_box");
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter().clone();
        panel_frame(&p, rect);
        let (_, close) = header_with(ui, &p, rect, "Open URL", "url", false);
        if close {
            action = UrlBoxAction::Cancel;
        }

        // The text field: dark box, accent line along the bottom (Windows 11 style)
        let field = Rect::from_min_size(pos2(rect.left() + 16.0, rect.top() + HEAD_H + 14.0), vec2(w - 32.0, 34.0));
        p.rect_filled(field, 4.0, Color32::from_black_alpha(70));
        p.rect_stroke(field.shrink(0.5), 4.0, Stroke::new(1.0_f32, Color32::from_rgb(64, 64, 64)));
        p.with_clip_rect(Rect::from_min_max(pos2(field.left(), field.bottom() - 2.0), field.max))
            .rect_filled(field, 4.0, accent);
        let inner = Rect::from_min_max(pos2(field.left() + 10.0, field.top()), pos2(field.right() - 10.0, field.bottom() - 2.0));
        let mut child = ui.child_ui(inner, egui::Layout::left_to_right(egui::Align::Center));
        {
            let v = child.visuals_mut();
            v.selection.bg_fill = accent.gamma_multiply(0.45);
            v.selection.stroke = Stroke::new(1.0_f32, TEXT);
            v.text_cursor = Stroke::new(1.5_f32, TEXT);
            v.extreme_bg_color = Color32::TRANSPARENT;
        }
        let edit = egui::TextEdit::singleline(&mut b.text)
            .id(egui::Id::new(crate::online::URL_FIELD))
            .frame(false)
            .font(FontId::proportional(13.5))
            .text_color(TEXT)
            .hint_text(egui::RichText::new("https://\u{2026}").color(DIM))
            .desired_width(inner.width());
        let resp = child.add(edit);
        if b.focus {
            resp.request_focus();
            b.focus = false;
        }
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        p.text(
            pos2(field.left() + 2.0, field.bottom() + 16.0),
            Align2::LEFT_CENTER,
            "A YouTube link, or one from any other site yt-dlp supports.",
            FontId::proportional(12.0),
            DIM,
        );

        // Cancel / Open (Open is dimmed until the text is a link)
        let valid = crate::online::normalize_url(&b.text).is_some();
        let open_r = Rect::from_min_size(pos2(rect.right() - 16.0 - 84.0, rect.bottom() - 16.0 - 30.0), vec2(84.0, 30.0));
        let cancel_r = open_r.translate(vec2(-92.0, 0.0));
        if cap_sized(ui, &p, cancel_r, "Cancel", ui.id().with("url_cancel"), 13.0) {
            action = UrlBoxAction::Cancel;
        }
        let clicked = accent_button(ui, &p, open_r, "Open", ui.id().with("url_open"), valid, accent);
        if valid && (clicked || enter) {
            action = UrlBoxAction::Open(b.text.clone());
        }
    });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
    action
}

// ── The save card (Save As… in progress) ──────────────────────────────────────

pub enum SaveCardAction {
    None,
    /// The X: cancel a save that's running, or just close the card
    Close,
    ShowInFolder,
}

/// One line of text cut to `width` with "…"
fn elided(p: &egui::Painter, text: &str, font: FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 1, break_anywhere: true, overflow_character: Some('\u{2026}') };
    p.layout_job(job)
}

/// A small card in the top right while a link is being saved: what's happening,
/// the video's title, and a progress bar; once saved, "Show in folder". `alpha`
/// fades it with the title bar (it only stays put when there's news).
pub fn draw_save_card(ctx: &egui::Context, title: &str, stage: &crate::online::SaveStage, alpha: f32, accent: Color32) -> SaveCardAction {
    use crate::online::{SavePart, SaveStage};
    if alpha <= 0.02 {
        return SaveCardAction::None;
    }
    let screen = ctx.screen_rect();
    let w = 320.0_f32.min(screen.width() - 16.0);
    let done = matches!(stage, SaveStage::Done(_));
    // (once saved: a separator, then the Show in folder button under it)
    let h = if done { 138.0 } else { 96.0 };
    let rect = Rect::from_min_size(pos2(screen.right() - 8.0 - w, screen.top() + PANEL_TOP), vec2(w, h));
    let mut action = SaveCardAction::None;
    let (heading, detail, bar): (&str, String, Option<Option<f32>>) = match stage {
        SaveStage::Starting => ("Saving", "Looking up the video\u{2026}".to_owned(), Some(None)),
        SaveStage::Downloading(part, frac) => {
            let what = match part {
                SavePart::Video => "Downloading the video",
                SavePart::Audio => "Downloading the audio",
                SavePart::Both => "Downloading",
            };
            let text = match frac {
                Some(f) => format!("{what} \u{b7} {}%", (f * 100.0).floor() as u32),
                None => format!("{what}\u{2026}"),
            };
            ("Saving", text, Some(*frac))
        }
        SaveStage::Finishing(true) => ("Saving", "Joining the video and audio\u{2026}".to_owned(), Some(None)),
        SaveStage::Finishing(false) => ("Saving", "Finishing\u{2026}".to_owned(), Some(None)),
        SaveStage::Updating => ("Saving", "It didn't work: updating yt-dlp, then trying again\u{2026}".to_owned(), Some(None)),
        SaveStage::Cutting(frac) => {
            let text = match frac {
                Some(f) => format!("Cutting the clip \u{b7} {}%", (f * 100.0).floor() as u32),
                None => "Cutting the clip\u{2026}".to_owned(),
            };
            ("Saving", text, Some(*frac))
        }
        SaveStage::Done(path) => {
            let name = Path::new(path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            ("Saved", name, None)
        }
        SaveStage::Failed(why) => ("Couldn't save", why.clone(), None),
    };
    let id = egui::Id::new("save_card");
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        ui.set_opacity(alpha);
        ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter().clone();
        panel_frame(&p, rect);
        let left = rect.left() + 16.0;

        // Heading and the X
        p.text(pos2(left, rect.top() + 22.0), Align2::LEFT_CENTER, heading, semibold(13.5), TEXT);
        let xr = Rect::from_center_size(pos2(rect.right() - 20.0, rect.top() + 21.0), vec2(26.0, 26.0));
        let x = ui.interact(xr, ui.id().with("save_close"), Sense::click_and_drag());
        if let Some(c) = state_fill(&x) {
            p.rect_filled(xr, 4.0, c);
        }
        let c = xr.center();
        for (a, b) in [((-4.5, -4.5), (4.5, 4.5)), ((-4.5, 4.5), (4.5, -4.5))] {
            p.line_segment([c + vec2(a.0, a.1), c + vec2(b.0, b.1)], Stroke::new(1.2_f32, TEXT));
        }
        let tip = if matches!(stage, SaveStage::Done(_) | SaveStage::Failed(_)) { "Close" } else { "Cancel saving" };
        if activated(&x.on_hover_text(tip)) {
            action = SaveCardAction::Close;
        }

        // The video's title
        let g = elided(&p, title, FontId::proportional(13.0), Color32::from_rgb(222, 222, 222), w - 32.0);
        p.galley(pos2(left, rect.top() + 38.0), g, Color32::from_rgb(222, 222, 222));

        // What's happening / the file's name / why it failed
        let col = if matches!(stage, SaveStage::Failed(_)) { Color32::from_rgb(232, 150, 140) } else { DIM };
        let g = elided(&p, &detail, FontId::proportional(12.0), col, w - 32.0);
        p.galley(pos2(left, rect.top() + 67.0 - g.size().y / 2.0), g, col);

        // Saved: a separator, then "Show in folder"
        if done {
            let y = rect.top() + 88.0;
            p.line_segment([pos2(rect.left() + 1.0, y), pos2(rect.right() - 1.0, y)], Stroke::new(1.0_f32, LINE));
            let br = Rect::from_min_max(pos2(rect.left() + 16.0, rect.bottom() - 12.0 - 30.0), pos2(rect.right() - 16.0, rect.bottom() - 12.0));
            if accent_button(ui, &p, br, "Show in folder", ui.id().with("save_show"), true, accent) {
                action = SaveCardAction::ShowInFolder;
            }
        }

        // Progress: a bar filling up, or a short piece sliding along while there's
        // nothing to measure (looking up, joining)
        if let Some(frac) = bar {
            let track = Rect::from_min_max(pos2(left, rect.bottom() - 12.0), pos2(rect.right() - 16.0, rect.bottom() - 9.0));
            crate::controls::pill(&p, track, Color32::from_rgb(70, 70, 72));
            match frac {
                Some(f) => {
                    if f > 0.0 {
                        let r = Rect::from_min_max(track.min, pos2(track.left() + track.width() * f, track.bottom()));
                        crate::controls::pill(&p, r.intersect(track), accent);
                    }
                }
                None => {
                    let t = ctx.input(|i| i.time) as f32;
                    let seg = track.width() * 0.28;
                    let x0 = track.left() - seg + (t * 0.8).fract() * (track.width() + seg);
                    let r = Rect::from_min_max(pos2(x0.max(track.left()), track.top()), pos2((x0 + seg).min(track.right()), track.bottom()));
                    if r.width() > 1.0 {
                        crate::controls::pill(&p, r, accent);
                    }
                    ctx.request_repaint();
                }
            }
        }
    });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
    action
}

// ── Save clip as… panel ───────────────────────────────────────────────────────

pub enum ClipAction {
    None,
    Save,
    Close,
}

/// The clip panel, top centre: its length, the start and end (click a time to
/// type one; "Set to now" takes the playhead's), "Play the clip on repeat", and
/// Cancel / Save clip…
pub fn draw_clip_panel(ctx: &egui::Context, c: &mut crate::online::ClipUi, duration: f64, now: f64, accent: Color32) -> ClipAction {
    use crate::online::{fmt_tenths, parse_time, ClipEnd, CLIP_FIELD};
    let screen = ctx.screen_rect();
    let w = 500.0_f32.min(screen.width() - 16.0);
    let h = 40.0 + 12.0 + 40.0 + 12.0 + 30.0 + 14.0;
    let rect = Rect::from_min_size(pos2(screen.center().x - w / 2.0, screen.top() + PANEL_TOP), vec2(w, h));
    let mut action = ClipAction::None;
    let id = egui::Id::new("clip_panel");
    egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter().clone();
        panel_frame(&p, rect);

        // Heading, the clip's length, X
        let cy = rect.top() + 20.0;
        let g = p.layout_no_wrap("Save a clip".to_owned(), semibold(14.0), TEXT);
        let after = rect.left() + 16.0 + g.size().x + 10.0;
        p.galley(pos2(rect.left() + 16.0, cy - g.size().y / 2.0), g, TEXT);
        let len = c.end - c.start;
        let len_text = if len >= 60.0 { format!("{}:{:04.1}", (len / 60.0).floor() as u64, len % 60.0) } else { format!("{len:.1} s") };
        p.text(pos2(after, cy), Align2::LEFT_CENTER, len_text, semibold(12.5), accent);
        let xr = Rect::from_center_size(pos2(rect.right() - 20.0, cy), vec2(26.0, 26.0));
        let x = ui.interact(xr, ui.id().with("clip_close"), Sense::click_and_drag());
        if let Some(f) = state_fill(&x) {
            p.rect_filled(xr, 4.0, f);
        }
        let xc = xr.center();
        for (a, b) in [((-4.5, -4.5), (4.5, 4.5)), ((-4.5, 4.5), (4.5, -4.5))] {
            p.line_segment([xc + vec2(a.0, a.1), xc + vec2(b.0, b.1)], Stroke::new(1.2_f32, TEXT));
        }
        if activated(&x.on_hover_text("Cancel (Esc)")) {
            action = ClipAction::Close;
        }
        p.line_segment([pos2(rect.left() + 1.0, rect.top() + 40.0), pos2(rect.right() - 1.0, rect.top() + 40.0)], Stroke::new(1.0_f32, LINE));

        // Start and end boxes
        let top = rect.top() + 52.0;
        let half = (w - 28.0 - 10.0) / 2.0;
        for (k, which) in [ClipEnd::Start, ClipEnd::End].into_iter().enumerate() {
            let bx = Rect::from_min_size(pos2(rect.left() + 14.0 + k as f32 * (half + 10.0), top), vec2(half, 40.0));
            p.rect_filled(bx, 6.0, Color32::from_black_alpha(55));
            p.rect_stroke(bx.shrink(0.5), 6.0, Stroke::new(1.0_f32, Color32::from_rgb(58, 58, 60)));
            let label = if which == ClipEnd::Start { "Start" } else { "End" };
            p.text(pos2(bx.left() + 10.0, bx.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.0), DIM);

            // "Set to now" ([ / ])
            let key = if which == ClipEnd::Start { "[" } else { "]" };
            let br = Rect::from_min_size(pos2(bx.right() - 6.0 - 98.0, bx.center().y - 13.0), vec2(98.0, 26.0));
            if cap_sized(ui, &p, br, &format!("Set to now  {key}"), ui.id().with(("clip_now", k)), 12.0) {
                c.set(which, now, duration);
                c.editing = None;
            }

            // The time: click it to type one
            let vr = Rect::from_min_max(pos2(bx.left() + 46.0, bx.top() + 6.0), pos2(br.left() - 8.0, bx.bottom() - 6.0));
            let typing_here = c.editing.as_ref().is_some_and(|(e, _)| *e == which);
            if typing_here {
                p.rect_filled(vr, 4.0, Color32::from_black_alpha(70));
                p.with_clip_rect(Rect::from_min_max(pos2(vr.left(), vr.bottom() - 2.0), vr.max)).rect_filled(vr, 4.0, accent);
                let mut child = ui.child_ui(vr.shrink2(vec2(6.0, 2.0)), egui::Layout::left_to_right(egui::Align::Center));
                {
                    let v = child.visuals_mut();
                    v.selection.bg_fill = accent.gamma_multiply(0.45);
                    v.text_cursor = Stroke::new(1.5_f32, TEXT);
                    v.extreme_bg_color = Color32::TRANSPARENT;
                }
                let mut done = None;
                if let Some((_, text)) = c.editing.as_mut() {
                    let resp = child.add(
                        egui::TextEdit::singleline(text)
                            .id(egui::Id::new(CLIP_FIELD))
                            .frame(false)
                            .font(semibold(15.0))
                            .text_color(TEXT)
                            .desired_width(vr.width() - 12.0),
                    );
                    if !resp.has_focus() && !resp.lost_focus() {
                        resp.request_focus();
                    }
                    // Enter, or clicking elsewhere, takes the typed time
                    if resp.lost_focus() {
                        done = Some(parse_time(text));
                    }
                }
                if let Some(t) = done {
                    if let Some(t) = t {
                        c.set(which, t, duration);
                    }
                    c.editing = None;
                }
            } else {
                let vresp = ui.interact(vr, ui.id().with(("clip_time", k)), Sense::click());
                if vresp.hovered() {
                    p.rect_filled(vr, 4.0, HOVER);
                    ctx.set_cursor_icon(egui::CursorIcon::Text);
                }
                p.text(pos2(vr.left() + 6.0, vr.center().y), Align2::LEFT_CENTER, fmt_tenths(c.time(which)), semibold(15.0), TEXT);
                if vresp.clicked() {
                    c.editing = Some((which, fmt_tenths(c.time(which))));
                }
            }
        }

        // "Play the clip on repeat", Cancel, Save clip…
        let row = rect.top() + 104.0;
        let cb = Rect::from_min_size(pos2(rect.left() + 14.0, row), vec2(210.0, 30.0));
        let cr = ui.interact(cb, ui.id().with("clip_repeat"), Sense::click_and_drag());
        if let Some(f) = state_fill(&cr) {
            p.rect_filled(cb, 4.0, f);
        }
        let bx = Rect::from_center_size(pos2(cb.left() + 14.0, cb.center().y), vec2(16.0, 16.0));
        if c.repeat {
            p.rect_filled(bx, 4.0, accent);
            p.add(egui::Shape::line(
                vec![bx.center() + vec2(-4.0, 0.0), bx.center() + vec2(-1.0, 3.0), bx.center() + vec2(4.5, -3.5)],
                Stroke::new(1.6_f32, Color32::WHITE),
            ));
        } else {
            p.rect_stroke(bx.shrink(0.5), 4.0, Stroke::new(1.0_f32, Color32::from_rgb(150, 150, 150)));
        }
        p.text(pos2(bx.right() + 9.0, cb.center().y), Align2::LEFT_CENTER, "Play the clip on repeat", FontId::proportional(12.5), TEXT);
        if activated(&cr) {
            c.repeat = !c.repeat;
        }
        let save_r = Rect::from_min_size(pos2(rect.right() - 14.0 - 116.0, row), vec2(116.0, 30.0));
        let cancel_r = save_r.translate(vec2(-124.0, 0.0));
        if cap_sized(ui, &p, cancel_r, "Cancel", ui.id().with("clip_cancel"), 13.0) {
            action = ClipAction::Close;
        }
        if accent_button(ui, &p, save_r, "Save clip\u{2026}", ui.id().with("clip_save"), c.end > c.start, accent) {
            action = ClipAction::Save;
        }
    });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
    action
}
