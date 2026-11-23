//! What fills the window while a song plays: the "glow bars" visualizer, or the
//! song's cover with a strip of bars under it (Picture & Sound → Sound → Show).
//!
//! mpv doesn't share the sound it plays, so the song is decoded a second time here
//! (with Symphonia), only to see its frequencies. A thread keeps about a second of
//! the song around the playing position; every frame the bars take a frequency
//! snapshot (FFT) at the playing time, so they stay in step with mpv, also after a
//! seek. Files Symphonia can't read (Opus, WMA, links...) just show the song's name.
//!
//! Drawn like the start screen: vertex-coloured meshes with additive "light", the
//! colours running teal → accent → violet. The backdrop is built once per window
//! size and reused, so a frame is only the bars: a few thousand vertices.

use crate::splash::{self, Rgb};
use eframe::egui::{self, pos2, vec2, Color32, FontFamily, FontId, Mesh, Rect};
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Number of bars
pub(crate) const BARS: usize = 64;
/// The frequencies the bars cover, spaced evenly in pitch (like a piano's keys)
const LO_HZ: f32 = 35.0;
const HI_HZ: f32 = 16_000.0;
/// Seconds of the song kept decoded ahead of / behind the playing time
const AHEAD: f64 = 0.4;
const BEHIND: f64 = 0.3;
/// How many dB below the loudest bar a bar reaches zero
const RANGE_DB: f32 = 42.0;
/// Rounding of the bars' top corners
const BAR_RADIUS: f32 = 4.0;

// ── The second decoder ────────────────────────────────────────────────────────

/// Whether the song can be shown as bars
#[derive(Clone, Copy, PartialEq, Default, Debug)]
pub(crate) enum State {
    /// Opening it, or no sound decoded at the playing time yet
    #[default]
    Waiting,
    Ready,
    /// Symphonia can't read this file
    Unsupported,
}

#[derive(Default)]
struct Want {
    /// The song to decode ("" = none)
    path: String,
    /// The playing time
    time: f64,
}

#[derive(Default)]
struct Buffer {
    /// The song these samples are from
    path: String,
    state: State,
    rate: u32,
    /// Time of data[0]
    start: f64,
    /// Mono samples
    data: VecDeque<f32>,
}

struct Shared {
    want: Mutex<Want>,
    wake: Condvar,
    buf: Mutex<Buffer>,
}

/// Decodes the playing song on its own thread, near the playing time
pub(crate) struct Scope {
    shared: Arc<Shared>,
}

impl Scope {
    pub(crate) fn new() -> Self {
        let shared = Arc::new(Shared { want: Mutex::new(Want::default()), wake: Condvar::new(), buf: Mutex::new(Buffer::default()) });
        let s = shared.clone();
        let _ = std::thread::Builder::new().name("visualizer".into()).spawn(move || worker(&s));
        Self { shared }
    }

    /// Keep decoding `path` around `time` (called every frame while bars are shown)
    pub(crate) fn follow(&self, path: &str, time: f64) {
        let mut w = self.shared.want.lock().unwrap();
        let changed = w.path != path;
        if changed {
            w.path = path.to_owned();
        }
        let jumped = (time - w.time).abs() > 0.5;
        w.time = time;
        if changed || jumped {
            self.shared.wake.notify_one();
        }
    }

    /// Stop decoding (no song, or nothing shows bars)
    pub(crate) fn stop(&self) {
        let mut w = self.shared.want.lock().unwrap();
        if !w.path.is_empty() {
            w.path.clear();
            self.shared.wake.notify_one();
        }
    }

    /// The song's state and sample rate
    fn status(&self, path: &str) -> (State, u32) {
        let b = self.shared.buf.lock().unwrap();
        if b.path != path {
            return (State::Waiting, 0);
        }
        (b.state, b.rate)
    }

    /// `out.len()` samples centred on `time`; false if they aren't decoded (yet)
    fn read(&self, path: &str, time: f64, out: &mut [f32]) -> bool {
        let b = self.shared.buf.lock().unwrap();
        if b.path != path || b.state != State::Ready || b.rate == 0 {
            return false;
        }
        let n = out.len() as i64;
        let first = ((time - b.start) * b.rate as f64).round() as i64 - n / 2;
        if first < 0 || first + n > b.data.len() as i64 {
            return false;
        }
        for (o, s) in out.iter_mut().zip(b.data.range(first as usize..(first + n) as usize)) {
            *o = *s;
        }
        true
    }
}

fn worker(sh: &Shared) {
    let mut src: Option<Source> = None;
    let mut mono = Vec::new();
    loop {
        // What to decode (sleep while nothing)
        let (path, time) = {
            let mut w = sh.want.lock().unwrap();
            while w.path.is_empty() {
                if src.take().is_some() {
                    *sh.buf.lock().unwrap() = Buffer::default();
                }
                w = sh.wake.wait(w).unwrap();
            }
            (w.path.clone(), w.time)
        };

        // A new song: open it
        if src.as_ref().map(|s| s.path.as_str()) != Some(path.as_str()) {
            src = None;
            let opened = Source::open(&path);
            let mut b = sh.buf.lock().unwrap();
            *b = Buffer { path: path.clone(), ..Buffer::default() };
            match opened {
                Some(s) => {
                    b.state = State::Ready;
                    b.rate = s.rate;
                    src = Some(s);
                }
                None => b.state = State::Unsupported,
            }
        }
        let Some(s) = src.as_mut() else {
            // (can't read it: wait for the next song)
            let w = sh.want.lock().unwrap();
            let _ = sh.wake.wait_timeout(w, Duration::from_millis(500));
            continue;
        };

        // What's decoded already
        let rate = s.rate as f64;
        let (start, end) = {
            let b = sh.buf.lock().unwrap();
            (b.start, b.start + b.data.len() as f64 / rate)
        };
        let empty = end <= start;
        // The playing time jumped (a seek): start again from there
        if empty || time < start || time > end + 1.0 {
            s.seek((time - 0.15).max(0.0));
            sh.buf.lock().unwrap().data.clear();
        }
        let end = if empty || time < start || time > end + 1.0 { f64::MIN } else { end };

        if end < time + AHEAD {
            let mut got = false;
            for _ in 0..8 {
                let Some(t) = s.next(&mut mono) else { break };
                got = true;
                let mut b = sh.buf.lock().unwrap();
                let expected = b.start + b.data.len() as f64 / rate;
                // (the first packet, or a gap: begin the buffer here)
                if b.data.is_empty() || (t - expected).abs() > 0.05 {
                    b.data.clear();
                    b.start = t;
                }
                b.data.extend(mono.iter().copied());
                // Drop what's well behind the playing time
                let keep_from = time - BEHIND;
                if keep_from > b.start {
                    let n = (((keep_from - b.start) * rate) as usize).min(b.data.len());
                    b.data.drain(..n);
                    b.start += n as f64 / rate;
                }
                if b.start + b.data.len() as f64 / rate >= time + AHEAD {
                    break;
                }
            }
            if !got {
                // (the end of the song, or a damaged part: try again shortly)
                let w = sh.want.lock().unwrap();
                let _ = sh.wake.wait_timeout(w, Duration::from_millis(50));
            }
        } else {
            // Enough decoded ahead: look again in a moment (sooner after a seek or a
            // new song, which wake this up)
            let w = sh.want.lock().unwrap();
            let _ = sh.wake.wait_timeout(w, Duration::from_millis(60));
        }
    }
}

/// An open song, decoded to mono
struct Source {
    path: String,
    format: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    track: u32,
    rate: u32,
    time_base: symphonia::core::units::TimeBase,
    interleaved: Vec<f32>,
}

impl Source {
    fn open(path: &str) -> Option<Source> {
        use symphonia::core::codecs::audio::AudioDecoderOptions;
        use symphonia::core::codecs::CodecParameters;
        use symphonia::core::formats::probe::Hint;
        use symphonia::core::formats::{FormatOptions, TrackType};
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::meta::MetadataOptions;

        if crate::online::is_url(path) {
            return None;
        }
        let file = std::fs::File::open(path).ok()?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
            .ok()?;
        let track = format.default_track(TrackType::Audio)?;
        let Some(CodecParameters::Audio(params)) = &track.codec_params else { return None };
        let rate = params.sample_rate?;
        let decoder = symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default()).ok()?;
        let time_base = track
            .time_base
            .or_else(|| symphonia::core::units::TimeBase::try_new(1, rate))?;
        let id = track.id;
        Some(Source { path: path.to_owned(), format, decoder, track: id, rate, time_base, interleaved: Vec::new() })
    }

    fn seek(&mut self, secs: f64) {
        use symphonia::core::formats::{SeekMode, SeekTo};
        if let Some(time) = symphonia::core::units::Time::try_from_secs_f64(secs) {
            let _ = self.format.seek(SeekMode::Coarse, SeekTo::Time { time, track_id: Some(self.track) });
        }
        self.decoder.reset();
    }

    /// The next piece of sound (mono into `mono`) and its start time; None at the
    /// end of the song or if it can't be read further
    fn next(&mut self, mono: &mut Vec<f32>) -> Option<f64> {
        use symphonia::core::errors::Error;
        for _ in 0..64 {
            let packet = self.format.next_packet().ok()??;
            if packet.track_id != self.track {
                continue;
            }
            let t = self.time_base.calc_time(packet.pts)?.as_secs_f64();
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let ch = buf.spec().channels().count().max(1);
                    buf.copy_to_vec_interleaved(&mut self.interleaved);
                    mono.clear();
                    let k = 1.0 / ch as f32;
                    mono.extend(self.interleaved.chunks_exact(ch).map(|f| f.iter().sum::<f32>() * k));
                    return Some(t);
                }
                // (one damaged piece: skip it)
                Err(Error::DecodeError(_)) => continue,
                Err(_) => return None,
            }
        }
        None
    }
}

// ── The bars ──────────────────────────────────────────────────────────────────

/// The bars' heights (0…1), moved on every frame
pub(crate) struct Bars {
    fft: Option<Arc<dyn RealToComplex<f32>>>,
    /// FFT size and the sample rate it's set up for
    n: usize,
    rate: u32,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    window: Vec<f32>,
    window_sum: f32,
    /// Each bar's frequency range in FFT bins, and its middle frequency
    bands: Vec<(f32, f32, f32)>,
    level: [f32; BARS],
    peak: [f32; BARS],
    /// Seconds a peak cap stays up before it falls, and how fast it's falling
    hold: [f32; BARS],
    fall: [f32; BARS],
    /// The loudness (dB) the tallest bars are measured against: follows the
    /// music, so quiet and loud songs both fill the height
    reference: f32,
    pub(crate) state: State,
    /// The backdrop and vignette, built for one window size / accent / height
    cache: Option<(Rect, Color32, f32, Mesh, Mesh)>,
}

impl Bars {
    pub(crate) fn new() -> Self {
        Self {
            fft: None,
            n: 0,
            rate: 0,
            input: Vec::new(),
            spectrum: Vec::new(),
            scratch: Vec::new(),
            window: Vec::new(),
            window_sum: 1.0,
            bands: Vec::new(),
            level: [0.0; BARS],
            peak: [0.0; BARS],
            hold: [0.0; BARS],
            fall: [0.0; BARS],
            reference: -20.0,
            state: State::Waiting,
            cache: None,
        }
    }

    /// The bars' heights (the playlist's little "playing" mark follows them)
    pub(crate) fn levels(&self) -> &[f32; BARS] {
        &self.level
    }

    /// A new song: the bars stay where they are and settle by themselves (the
    /// loudness is measured afresh)
    pub(crate) fn soften(&mut self) {
        self.reference = -20.0;
        self.state = State::Waiting;
    }

    /// Set up the FFT for a sample rate (about 0.09 s of sound per snapshot)
    fn prepare(&mut self, rate: u32) {
        if rate == self.rate && self.fft.is_some() {
            return;
        }
        let n = if rate > 60_000 { 8192 } else { 4096 };
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        self.input = fft.make_input_vec();
        self.spectrum = fft.make_output_vec();
        self.scratch = fft.make_scratch_vec();
        // Hann window
        self.window = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
        self.window_sum = self.window.iter().sum();
        let bin_hz = rate as f32 / n as f32;
        let hi = HI_HZ.min(rate as f32 * 0.45);
        self.bands = (0..BARS)
            .map(|i| {
                let f = |k: f32| LO_HZ * (hi / LO_HZ).powf(k / BARS as f32);
                let (lo, top) = (f(i as f32), f(i as f32 + 1.0));
                (lo / bin_hz, top / bin_hz, (lo * top).sqrt())
            })
            .collect();
        self.fft = Some(fft);
        self.n = n;
        self.rate = rate;
    }

    /// Move the bars on to `time` (seconds into the song). Returns true while they
    /// are moving, i.e. while the window should keep redrawing.
    pub(crate) fn update(&mut self, scope: &Scope, path: &str, time: f64, playing: bool, dt: f32) -> bool {
        let dt = dt.clamp(0.0, 0.1);
        let mut target = [0.0_f32; BARS];
        let (state, rate) = scope.status(path);
        self.state = state;
        if playing && state == State::Ready && rate > 0 {
            self.prepare(rate);
            let mut input = std::mem::take(&mut self.input);
            if scope.read(path, time, &mut input) {
                self.measure(&mut input, &mut target, dt);
            }
            self.input = input;
        }

        // Fast up, gentle down; the peak caps wait a moment, then drop with gravity
        let up = 1.0 - (-dt * 38.0).exp();
        let down = 1.0 - (-dt * 7.0).exp();
        let mut moving = playing && state == State::Ready;
        for (i, &target) in target.iter().enumerate() {
            let k = if target > self.level[i] { up } else { down };
            self.level[i] += (target - self.level[i]) * k;
            if self.level[i] >= self.peak[i] {
                self.peak[i] = self.level[i];
                self.hold[i] = 0.35;
                self.fall[i] = 0.0;
            } else if self.hold[i] > 0.0 {
                self.hold[i] -= dt;
            } else {
                self.fall[i] += dt * 1.8;
                self.peak[i] = (self.peak[i] - self.fall[i] * dt).max(self.level[i]);
            }
            if self.level[i] > 0.002 || self.peak[i] > 0.002 {
                moving = true;
            }
        }
        moving
    }

    /// One frequency snapshot of `input` → bar heights
    fn measure(&mut self, input: &mut [f32], target: &mut [f32; BARS], dt: f32) {
        let Some(fft) = self.fft.clone() else { return };
        for (s, w) in input.iter_mut().zip(&self.window) {
            *s *= w;
        }
        if fft.process_with_scratch(input, &mut self.spectrum, &mut self.scratch).is_err() {
            return;
        }
        // Amplitude of a full-scale sine wave = 1.0
        let norm = 2.0 / self.window_sum;
        let mag = |k: usize| self.spectrum.get(k).map_or(0.0, |c| c.norm() * norm);
        let mut db = [0.0_f32; BARS];
        let mut loudest = f32::MIN;
        for (i, &(lo, hi, mid_hz)) in self.bands.iter().enumerate() {
            let amp = if hi - lo < 1.0 {
                // A band narrower than one FFT step (the bass): read between the steps
                let c = (lo + hi) / 2.0;
                let (k, f) = (c.floor() as usize, c.fract());
                mag(k) * (1.0 - f) + mag(k + 1) * f
            } else {
                // Wider bands: their average power
                let (a, b) = (lo.ceil() as usize, (hi.floor() as usize).max(lo.ceil() as usize));
                let sum: f32 = (a..=b).map(|k| mag(k).powi(2)).sum();
                (sum / (b - a + 1) as f32).sqrt()
            };
            // + 3 dB per octave: music has less energy up high, and without this the
            // right half of the bars would hardly move
            db[i] = 20.0 * (amp + 1e-9).log10() + 3.0 * (mid_hz / 1000.0).log2();
            loudest = loudest.max(db[i]);
        }
        // The reference follows the loudest bar: up quickly, down slowly (so a
        // quiet passage looks quieter, but a quiet song still fills the height)
        let k = if loudest > self.reference { 1.0 - (-dt * 6.0).exp() } else { 1.0 - (-dt * 0.35).exp() };
        self.reference = (self.reference + (loudest - self.reference) * k).max(-50.0);
        let top = self.reference + 2.0;
        for (t, d) in target.iter_mut().zip(db) {
            let v = ((d - (top - RANGE_DB)) / RANGE_DB).clamp(0.0, 1.0);
            *t = v * v * (3.0 - 2.0 * v); // (eases the low end, so quiet bands don't flicker)
        }
    }

    // ── Drawing ───────────────────────────────────────────────────────────────

    /// The start screen's background (grid, accent glow) centred on height `cy`;
    /// `vignette` is drawn afterwards, over everything
    fn backdrop(&mut self, p: &egui::Painter, rect: Rect, accent: Color32, cy: f32) {
        let fresh = !matches!(&self.cache, Some((r, a, y, _, _)) if *r == rect && *a == accent && *y == cy);
        if fresh {
            let mut back = Mesh::default();
            // (the grid a little stronger than the start screen's: behind the bars it
            // was hard to see)
            splash::backdrop(&mut back, rect, accent, cy, 1.6);
            let mut vig = Mesh::default();
            splash::vignette(&mut vig, rect);
            self.cache = Some((rect, accent, cy, back, vig));
        }
        if let Some((_, _, _, back, _)) = &self.cache {
            p.add(egui::Shape::mesh(back.clone()));
        }
    }

    fn vignette(&self, p: &egui::Painter) {
        if let Some((_, _, _, _, vig)) = &self.cache {
            p.add(egui::Shape::mesh(vig.clone()));
        }
    }

    /// The bars standing on `area.bottom()`, at most `area.height()` tall.
    /// `reflect`: a faint mirror image under the line; `glow`: the soft halo's size.
    fn draw_bars(&self, p: &egui::Painter, area: Rect, accent: Color32, reflect: bool, glow: f32) {
        let acc = splash::rgb(accent);
        let mut m = Mesh::default();
        let gap = (area.width() * 0.0035).max(1.0);
        let bw = ((area.width() - gap * (BARS as f32 - 1.0)) / BARS as f32).max(1.0);
        let base = area.bottom();
        let white: Rgb = [1.0, 1.0, 1.0];
        for i in 0..BARS {
            let t = i as f32 / (BARS - 1) as f32;
            let c = splash::ribbon_colour(acc, t);
            let x = area.left() + i as f32 * (bw + gap);
            let bh = (self.level[i] * area.height()).max(2.0);
            let bar = Rect::from_min_max(pos2(x, base - bh), pos2(x + bw, base));

            // Halo: brightest where the bar is tallest
            let g = glow * (0.5 + 0.5 * self.level[i]);
            soft_rect(&mut m, bar, g, g, g * 1.3, 0.0, splash::light(c, 0.10 + 0.12 * self.level[i]));

            // The bar: lighter at the top, its top corners slightly rounded
            let top_col = splash::light(splash::mix(c, white, 0.25), 0.95);
            let bot_col = splash::light(c, 0.55);
            let r = BAR_RADIUS.min(bw / 2.0).min(bh / 2.0);
            let body = Rect::from_min_max(pos2(bar.left(), bar.top() + r), bar.max);
            quad_v(&mut m, body, top_col, bot_col);
            rounded_top(&mut m, bar, r, top_col);

            // Mirror image fading out under the line
            if reflect {
                let rh = bh * 0.55;
                let refl = Rect::from_min_max(pos2(bar.left(), base + 3.0), pos2(bar.right(), base + 3.0 + rh));
                quad_v(&mut m, refl, splash::light(c, 0.26), Color32::TRANSPARENT);
            }

            // Peak cap, drifting down after a hit
            if self.peak[i] > 0.01 {
                let y = base - (self.peak[i] * area.height()).max(2.0) - 5.0;
                let pk = Rect::from_min_max(pos2(bar.left(), y - 2.0), pos2(bar.right(), y));
                quad_v(&mut m, pk, splash::light(splash::mix(c, white, 0.5), 0.85), splash::light(splash::mix(c, white, 0.5), 0.85));
            }
        }
        // The line the bars stand on
        let line = Rect::from_min_max(pos2(area.left(), base + 1.0), pos2(area.right(), base + 2.0));
        quad_v(&mut m, line, splash::light(acc, 0.22), splash::light(acc, 0.22));
        p.add(egui::Shape::mesh(m));
    }

    /// The visualizer filling `rect`, with the song's name at the bottom middle
    pub(crate) fn draw_visualizer(&mut self, p: &egui::Painter, rect: Rect, name: SongName, accent: Color32) {
        let p = p.with_clip_rect(rect);
        let h = rect.height();
        let base = rect.top() + h * 0.58;
        self.backdrop(&p, rect, accent, base);
        let s = (h / 720.0).clamp(0.45, 2.0);
        // (edge to edge)
        let area = Rect::from_min_max(pos2(rect.left(), base - h * 0.34), pos2(rect.right(), base));
        self.draw_bars(&p, area, accent, true, 14.0 * s);
        self.vignette(&p);
        let l = CoverLayout::new(rect, name.lines());
        song_name(&p, rect.center().x, l.name_top, rect.width() * 0.8, &name, l.scale);
    }

    /// Where the cover goes on the album-art screen (None: the window is too small
    /// for it, show the visualizer instead). `aspect` = the cover's width / height.
    /// `lines`: how many lines the song's name takes (SongName::lines)
    pub(crate) fn cover_rect(rect: Rect, aspect: f32, lines: usize) -> Option<Rect> {
        let l = CoverLayout::new(rect, lines);
        let side = (l.cover_bottom - l.cover_top).min(rect.width() * 0.6);
        if side < 90.0 {
            return None;
        }
        let aspect = if aspect.is_finite() && aspect > 0.2 { aspect.clamp(0.5, 2.0) } else { 1.0 };
        let size = if aspect >= 1.0 { vec2(side, side / aspect) } else { vec2(side * aspect, side) };
        let centre = pos2(rect.center().x, l.cover_bottom - side / 2.0);
        let r = Rect::from_center_size(centre, size);
        Some(Rect::from_min_max(r.min.round(), r.max.round()))
    }

    /// The album-art screen, behind the cover (which mpv draws into `cover` after this)
    pub(crate) fn draw_cover_back(&mut self, p: &egui::Painter, rect: Rect, cover: Rect, accent: Color32) {
        let p = p.with_clip_rect(rect);
        self.backdrop(&p, rect, accent, cover.center().y);
        // A soft shadow under the cover
        let blur = cover.width() * 0.12;
        let shadow = egui::epaint::Shadow { offset: vec2(0.0, blur * 0.25), blur, spread: 0.0, color: Color32::from_black_alpha(170) };
        p.add(egui::Shape::mesh(shadow.tessellate(cover, 4.0)));
    }

    /// The album-art screen, around the cover: the song's name under it
    pub(crate) fn draw_cover_front(&mut self, p: &egui::Painter, rect: Rect, name: SongName) {
        let p = p.with_clip_rect(rect);
        let l = CoverLayout::new(rect, name.lines());
        song_name(&p, rect.center().x, l.name_top, rect.width() * 0.8, &name, l.scale);
        self.vignette(&p);
    }

    /// No pictures and no bars: the backdrop with the song's name
    pub(crate) fn draw_still(&mut self, p: &egui::Painter, rect: Rect, name: SongName, accent: Color32) {
        let p = p.with_clip_rect(rect);
        let cy = rect.top() + rect.height() * 0.46;
        self.backdrop(&p, rect, accent, cy);
        let s = (rect.height() / 720.0).clamp(0.9, 1.6);
        let top = cy - name.lines() as f32 * LINE_H * s / 2.0;
        song_name(&p, rect.center().x, top, rect.width() * 0.8, &name, s);
        self.vignette(&p);
    }
}

/// The album-art screen, top to bottom: the cover, then the song's name (just
/// above the controls)
struct CoverLayout {
    scale: f32,
    cover_top: f32,
    cover_bottom: f32,
    /// Top of the song's name
    name_top: f32,
}

impl CoverLayout {
    fn new(rect: Rect, lines: usize) -> Self {
        // (the text stays readable in a small window)
        let scale = (rect.height() / 720.0).clamp(0.9, 1.6);
        let name_top = rect.bottom() - 118.0 - lines as f32 * LINE_H * scale;
        Self { scale, cover_top: rect.top() + 56.0, cover_bottom: name_top - 22.0 * scale, name_top }
    }
}

/// Height of a line of the song's name (at the 720 p size)
const LINE_H: f32 = 23.0;

/// What's shown about the playing song: each line labelled ("Song: The Tourist",
/// "Artist: Radiohead"...), empty ones left out
pub(crate) struct SongName<'a> {
    pub(crate) song: &'a str,
    pub(crate) artist: &'a str,
    pub(crate) album: &'a str,
    /// "3 of 12" in a playlist
    pub(crate) playlist: Option<&'a str>,
}

impl SongName<'_> {
    fn rows(&self) -> Vec<(&'static str, &str)> {
        [("Song", self.song), ("Artist", self.artist), ("Album", self.album), ("Playlist", self.playlist.unwrap_or(""))]
            .into_iter()
            .map(|(k, v)| (k, v.trim()))
            .filter(|(_, v)| !v.is_empty())
            .collect()
    }

    pub(crate) fn lines(&self) -> usize {
        self.rows().len()
    }
}

/// The song's name as labelled lines: small labels right-aligned in a column
/// ("SONG", "ARTIST"...), the values left-aligned after it; the block centred on
/// `cx` with its top at `top`
fn song_name(p: &egui::Painter, cx: f32, top: f32, max_w: f32, name: &SongName, s: f32) {
    let rows = name.rows();
    let label_font = FontId::proportional(11.5 * s);
    let value_font = |first: bool| if first { FontId::new(15.5 * s, FontFamily::Name("semibold".into())) } else { FontId::proportional(15.0 * s) };
    let label_col = Color32::from_rgb(128, 136, 156);
    let value_col = Color32::from_rgb(236, 238, 245);
    let gap = 12.0 * s;
    let labels: Vec<_> = rows.iter().map(|(k, _)| p.layout_no_wrap(k.to_uppercase(), label_font.clone(), label_col)).collect();
    let label_w = labels.iter().map(|g| g.size().x).fold(0.0, f32::max);
    let values: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(i, (_, v))| crate::controls::elide(p, v, value_font(i == 0), value_col, (max_w - label_w - gap).max(40.0)))
        .collect();
    let block_w = label_w + gap + values.iter().map(|g| g.size().x).fold(0.0, f32::max);
    let gutter = (cx - block_w / 2.0 + label_w).round();
    for (i, (l, v)) in labels.into_iter().zip(values).enumerate() {
        let cy = top + (i as f32 + 0.5) * LINE_H * s;
        let lp = pos2(gutter - l.size().x, cy - l.size().y / 2.0 + 1.0 * s);
        let vp = pos2(gutter + gap, cy - v.size().y / 2.0);
        for (pos, g) in [(lp, &l), (vp, &v)] {
            p.galley_with_override_text_color(pos + vec2(1.0, 1.0), g.clone(), Color32::from_black_alpha(140));
        }
        p.galley(lp, l, label_col);
        p.galley(vp, v, value_col);
    }
}

/// A rectangle with a vertical colour gradient
fn quad_v(m: &mut Mesh, r: Rect, top: Color32, bottom: Color32) {
    let i = m.vertices.len() as u32;
    m.colored_vertex(r.left_top(), top);
    m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.right_bottom(), bottom);
    m.colored_vertex(r.left_bottom(), bottom);
    m.add_triangle(i, i + 1, i + 2);
    m.add_triangle(i, i + 2, i + 3);
}

/// The top `r` of a bar, with its two top corners rounded by `r`
fn rounded_top(m: &mut Mesh, bar: Rect, r: f32, col: Color32) {
    use std::f32::consts::{FRAC_PI_2, PI};
    const SEG: u32 = 4;
    let cy = bar.top() + r;
    // A fan from the middle of the strip's bottom edge, round the two corners
    let i = m.vertices.len() as u32;
    m.colored_vertex(pos2(bar.center().x, cy), col);
    for (cx, from) in [(bar.left() + r, PI), (bar.right() - r, FRAC_PI_2)] {
        for s in 0..=SEG {
            let a = from - FRAC_PI_2 * s as f32 / SEG as f32;
            m.colored_vertex(pos2(cx + r * a.cos(), cy - r * a.sin()), col);
        }
    }
    let n = 2 * (SEG + 1);
    for k in 1..n {
        m.add_triangle(i, i + k, i + k + 1);
    }
}

/// `col` filling `inner` and fading to nothing over the given margins (left,
/// right, top, bottom): a cheap soft glow or shadow
fn soft_rect(m: &mut Mesh, inner: Rect, l: f32, r: f32, t: f32, b: f32, col: Color32) {
    let xs = [inner.left() - l, inner.left(), inner.right(), inner.right() + r];
    let ys = [inner.top() - t, inner.top(), inner.bottom(), inner.bottom() + b];
    let base = m.vertices.len() as u32;
    for (yi, y) in ys.iter().enumerate() {
        for (xi, x) in xs.iter().enumerate() {
            let inside = (xi == 1 || xi == 2) && (yi == 1 || yi == 2);
            m.colored_vertex(pos2(*x, *y), if inside { col } else { Color32::TRANSPARENT });
        }
    }
    for yi in 0..3u32 {
        for xi in 0..3u32 {
            let a = base + yi * 4 + xi;
            m.add_triangle(a, a + 1, a + 5);
            m.add_triangle(a, a + 5, a + 4);
        }
    }
}

/// A song's title and artist tags and its length, for the playlist panel (None
/// for what isn't there or can't be read)
pub(crate) fn song_info(path: &str) -> (Option<String>, Option<String>, Option<f64>) {
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{MetadataOptions, StandardTag};

    let Ok(file) = std::fs::File::open(path) else { return (None, None, None) };
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let Ok(mut format) = symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default()) else {
        return (None, None, None);
    };
    let secs = format.default_track(TrackType::Audio).and_then(|t| {
        let rate = match &t.codec_params {
            Some(CodecParameters::Audio(a)) => a.sample_rate?,
            _ => return None,
        };
        let n = t.num_frames?;
        Some(n as f64 / rate as f64).filter(|s| *s > 0.0)
    });
    let (mut title, mut artist) = (None, None);
    let mut meta = format.metadata();
    if let Some(rev) = meta.skip_to_latest() {
        let tags = rev.media.tags.iter().chain(rev.per_track.iter().flat_map(|t| t.metadata.tags.iter()));
        for tag in tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(t)) if title.is_none() => title = Some(t.trim().to_owned()),
                Some(StandardTag::Artist(a)) if artist.is_none() => artist = Some(a.trim().to_owned()),
                _ => {}
            }
        }
    }
    (title.filter(|t| !t.is_empty()), artist.filter(|a| !a.is_empty()), secs)
}
