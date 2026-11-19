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

    /// Colour temperature and vibrance work on RGB frames, so with either on the
    /// video reaches mpv as RGB, and mpv's own colour controls then don't work
    /// properly on it (Contrast had no effect; Saturation and Hue only work on
    /// normal YUV video). In that case Brightness, Contrast, Saturation, Gamma and
    /// Hue are done by FFmpeg filters instead, first in the chain while the video
    /// is still YUV.
    pub fn cpu_colour(&self) -> bool {
        self.temperature != 0 || self.vibrance != 0
    }

    /// Values for mpv's own colour controls: zero while FFmpeg does them
    pub fn gpu_colour(&self) -> [(&'static str, i32); 5] {
        let k = if self.cpu_colour() { 0 } else { 1 };
        [
            ("brightness", self.brightness * k),
            ("contrast", self.contrast * k),
            ("saturation", self.saturation * k),
            ("gamma", self.gamma * k),
            ("hue", self.hue * k),
        ]
    }

    /// FFmpeg `eq` settings matching mpv's controls: (name, value)
    pub fn eq_values(&self) -> [(&'static str, String); 4] {
        [
            ("contrast", format!("{:.3}", 1.0 + self.contrast as f32 / 100.0)),
            ("brightness", format!("{:.3}", self.brightness as f32 / 100.0)),
            ("saturation", format!("{:.3}", 1.0 + self.saturation as f32 / 100.0)),
            ("gamma", format!("{:.3}", 8f32.powf(self.gamma as f32 / 100.0))),
        ]
    }

    /// Hue in degrees for FFmpeg's `hue` filter (mpv's ±100 is ±180°)
    pub fn hue_degrees(&self) -> String {
        format!("{:.1}", self.hue as f32 * 1.8)
    }

    /// mpv's `vf` list for the picture filters (plus mirroring), or None for no
    /// filters. With `with_values` false the colour values are left out: comparing
    /// those tells whether only colour values changed (then they're sent live).
    pub fn vf_chain(&self, mirror: bool, with_values: bool) -> Option<String> {
        let mut chain: Vec<String> = Vec::new();
        if self.cpu_colour() {
            let eq: Vec<String> = self
                .eq_values()
                .iter()
                .map(|(k, v)| format!("{k}={}", if with_values { v.as_str() } else { "_" }))
                .collect();
            chain.push(format!("@adjeq:lavfi=[eq={}]", eq.join(":")));
            chain.push(format!("@adjhue:lavfi=[hue=h={}]", if with_values { self.hue_degrees() } else { "_".to_owned() }));
        }
        let mut f: Vec<String> = Vec::new();
        if self.denoise > 0 {
            f.push(format!("hqdn3d={}", [0, 2, 4, 7][self.denoise as usize]));
        }
        if self.temperature != 0 {
            // 6500 K is neutral; lower is warmer
            f.push(format!("colortemperature=temperature={}", 6500 - self.temperature * 600));
        }
        if self.vibrance != 0 {
            f.push(format!("vibrance=intensity={:.3}", self.vibrance as f32 * 0.05));
        }
        if self.sharpen > 0 {
            f.push(format!("cas=strength={:.2}", self.sharpen as f32 * 0.2));
        }
        if mirror {
            f.push("hflip".to_owned());
        }
        if !f.is_empty() {
            chain.push(format!("lavfi=[{}]", f.join(",")));
        }
        (!chain.is_empty()).then(|| chain.join(","))
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
        }
    }

    pub fn contains(&self, p: Pos2) -> bool {
        (self.open && self.panel_rect.contains(p)) || (self.eq_open && self.eq_rect.contains(p))
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
pub fn state_fill(resp: &egui::Response) -> Option<Color32> {
    let inside = resp.ctx.input(|i| i.pointer.latest_pos()).map_or(false, |p| resp.rect.contains(p));
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

/// Title, optional "Reset" link and an X. Returns (reset clicked, close clicked).
fn header(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, title: &str, id: &str) -> (bool, bool) {
    let cy = rect.top() + HEAD_H / 2.0;
    p.text(pos2(rect.left() + 16.0, cy), Align2::LEFT_CENTER, title, semibold(14.5), TEXT);

    let xr = Rect::from_center_size(pos2(rect.right() - 22.0, cy), vec2(28.0, 28.0));
    let x = ui.interact(xr, ui.id().with((id, "close")), Sense::click());
    if let Some(c) = state_fill(&x) {
        p.rect_filled(xr, 4.0, c);
    }
    let c = xr.center();
    for (a, b) in [((-5.0, -5.0), (5.0, 5.0)), ((-5.0, 5.0), (5.0, -5.0))] {
        p.line_segment([c + vec2(a.0, a.1), c + vec2(b.0, b.1)], Stroke::new(1.2_f32, TEXT));
    }

    let font = FontId::proportional(12.5);
    let g = p.layout_no_wrap("Reset".to_owned(), font, DIM);
    let rr = Rect::from_center_size(pos2(xr.left() - 10.0 - g.size().x / 2.0, cy), g.size() + vec2(10.0, 8.0));
    let reset = ui.interact(rr, ui.id().with((id, "reset")), Sense::click());
    if let Some(c) = state_fill(&reset) {
        p.rect_filled(rr, 4.0, c);
    }
    let col = if reset.hovered() { TEXT } else { DIM };
    p.galley(rr.center() - g.size() / 2.0, g, col);

    p.line_segment(
        [pos2(rect.left() + 1.0, rect.top() + HEAD_H - 0.5), pos2(rect.right() - 1.0, rect.top() + HEAD_H - 0.5)],
        Stroke::new(1.0_f32, LINE),
    );
    (reset.clicked(), x.clicked())
}

/// A key cap button ("−" / "+") with its label at `size`; returns true when clicked
fn cap_sized(ui: &mut egui::Ui, p: &egui::Painter, rect: Rect, label: &str, id: egui::Id, size: f32) -> bool {
    let resp = ui.interact(rect, id, Sense::click());
    let inside = resp.ctx.input(|i| i.pointer.latest_pos()).map_or(false, |p| rect.contains(p));
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
    resp.clicked()
}

/// Windows 11-style on/off switch
fn switch(p: &egui::Painter, rect: Rect, on: bool, accent: Color32) {
    let r = rect.height() / 2.0;
    if on {
        p.rect_filled(rect, r, accent);
        p.circle_filled(pos2(rect.right() - r, rect.center().y), r - 4.5, Color32::from_rgb(26, 26, 26));
    } else {
        p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, Color32::from_rgb(154, 154, 154)));
        p.circle_filled(pos2(rect.left() + r, rect.center().y), r - 4.5, Color32::from_rgb(207, 207, 207));
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

fn rows(a: &Adjust, normalize: bool, hq: bool, tab: Tab) -> Vec<Row> {
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
                Row::Step("Vibrance", "Boosts dull colours more than already-vivid ones, so skin tones stay natural.", v, vc, StepKey::Vibrance),
                Row::Step("Color temperature", "Warmer (more orange) or cooler (more blue) white balance.", temp, a.temperature != 0, StepKey::Temperature),
                Row::Step("Sharpen", "Crisper edges (AMD's contrast-adaptive sharpening). High levels can look harsh on grainy video.", sharpen, a.sharpen != 0, StepKey::Sharpen),
                Row::Step("Denoise", "Smooths grain and compression noise. Uses a lot of processor time and may stutter on 4K video.", denoise, a.denoise != 0, StepKey::Denoise),
                Row::Sep,
                Row::Switch("Deband", "Smooths visible steps in gradients such as skies and dark scenes.", a.deband, SwitchKey::Deband),
                Row::Switch("Deinterlace", "Removes comb-like lines from old TV recordings and DVDs.", a.deinterlace, SwitchKey::Deinterlace),
                Row::Switch("Smooth motion", "Matches the video to your monitor's refresh rate, so panning shots in films judder less.", a.smooth, SwitchKey::Smooth),
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
    let list = rows(a, *normalize, *hq, ui_state.tab);
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
            let resp = ui.interact(tr, ui.id().with(("tab", i)), Sense::click());
            let on = ui_state.tab == tab;
            if on {
                // The selected tab: an accent shape with the tab drawn over it 2 px
                // higher, leaving an accent edge along the bottom that follows the
                // rounded corners up the sides
                p.rect_filled(tr, 5.0, accent);
                p.with_clip_rect(tr).rect_filled(tr.translate(vec2(0.0, -2.0)), 5.0, Color32::from_rgb(68, 68, 68));
            } else {
                let fill = state_fill(&resp).unwrap_or(Color32::from_rgba_premultiplied(8, 8, 8, 8));
                p.rect_filled(tr, 5.0, fill);
            }
            let (font, col) = if on { (semibold(NAME_SIZE), TEXT) } else { (FontId::proportional(NAME_SIZE), Color32::from_rgb(200, 200, 200)) };
            p.text(tr.center(), Align2::CENTER_CENTER, label, font, col);
            if resp.clicked() {
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
                        let resp = ui.interact(r, ui.id().with((*name, "switch")), Sense::click());
                        if let Some(c) = state_fill(&resp) {
                            p.rect_filled(r, 4.0, c);
                        }
                        if resp.hovered() {
                            hovered_desc = Some((r, desc));
                        }
                        p.text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, *name, FontId::proportional(NAME_SIZE), TEXT);
                        switch(&p, Rect::from_center_size(pos2(r.right() - 28.0, r.center().y), vec2(38.0, 19.0)), *on, accent);
                        if resp.clicked() {
                            flip(a, normalize, hq, *key);
                        }
                    }
                    Row::Equalizer(on) => {
                        let resp = ui.interact(r, ui.id().with("eq_button"), Sense::click());
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
                        if resp.clicked() {
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
    // A row's description appears after the pointer has been on that row for a
    // moment, then stays while the pointer is anywhere on the row (moving or not).
    // Moving to another row while one is showing switches straight to the new one.
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
        let sw = ui.interact(sw_row, ui.id().with("eq_on"), Sense::click());
        if let Some(c) = state_fill(&sw) {
            p.rect_filled(sw_row, 4.0, c);
        }
        switch(&p, Rect::from_min_size(pos2(sw_row.left() + 8.0, cy - 9.5), vec2(38.0, 19.0)), a.eq_on, accent);
        p.text(pos2(sw_row.left() + 54.0, cy), Align2::LEFT_CENTER, if a.eq_on { "On" } else { "Off" }, FontId::proportional(NAME_SIZE), TEXT);
        if sw.clicked() {
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
            p.circle_filled(pos2(tx, ky + 1.0), if hot { 7.3 } else { 6.6 }, Color32::from_black_alpha(80));
            p.circle_filled(pos2(tx, ky), if hot { 7.0 } else { 6.2 }, Color32::from_rgb(186, 186, 188));
            p.circle_filled(pos2(tx, ky), if hot { 4.2 } else { 3.6 }, accent.gamma_multiply(dim.max(0.7)));

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
