//! Idle screen shown while no video is open: the "audio ribbon" artwork.
//!
//! Everything is drawn as vertex-coloured meshes (no images), so it is sharp at any
//! window size, follows the Windows accent colour, and costs next to nothing: it is
//! only redrawn when something else on screen changes.
//!
//! Glows use "additive" colours (premultiplied colour with zero alpha), which brighten
//! what is underneath like light does, instead of painting over it.

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Mesh, Pos2, Rect};

type Rgb = [f32; 3];

const TEAL: Rgb = [0.10, 0.90, 0.80];
const VIOLET: Rgb = [0.70, 0.35, 1.00];
const GRID: Rgb = [0.60, 0.80, 1.00];

fn rgb(c: Color32) -> Rgb {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0]
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Additive light of colour `c` at strength `k`
fn light(c: Rgb, k: f32) -> Color32 {
    let ch = |v: f32| (v * k * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_premultiplied(ch(c[0]), ch(c[1]), ch(c[2]), 0)
}

/// Ribbon colour along the width: teal → accent → violet
fn ribbon_colour(accent: Rgb, t: f32) -> Rgb {
    if t < 0.5 {
        mix(TEAL, accent, t * 2.0)
    } else {
        mix(accent, VIOLET, (t - 0.5) * 2.0)
    }
}

/// Soft strip along a polyline: full colour on the line, fading to nothing at ±half_w
fn strip(mesh: &mut Mesh, pts: &[Pos2], half_w: f32, cols: &[Color32]) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let base = mesh.vertices.len() as u32;
    for i in 0..n {
        let a = pts[i.saturating_sub(1)];
        let b = pts[(i + 1).min(n - 1)];
        let d = (b - a).normalized();
        let nrm = vec2(-d.y, d.x) * half_w;
        mesh.colored_vertex(pts[i] - nrm, Color32::TRANSPARENT);
        mesh.colored_vertex(pts[i], cols[i]);
        mesh.colored_vertex(pts[i] + nrm, Color32::TRANSPARENT);
    }
    for i in 0..(n as u32 - 1) {
        let (l0, c0, r0) = (base + i * 3, base + i * 3 + 1, base + i * 3 + 2);
        let (l1, c1, r1) = (l0 + 3, c0 + 3, r0 + 3);
        mesh.add_triangle(l0, c0, l1);
        mesh.add_triangle(c0, c1, l1);
        mesh.add_triangle(c0, r0, c1);
        mesh.add_triangle(r0, r1, c1);
    }
}

/// Concentric rings around `c` (radii rx·d, ry·d); `ring(d)` gives each ring's colour
fn rings(mesh: &mut Mesh, c: Pos2, rx: f32, ry: f32, ds: &[f32], ring: impl Fn(f32) -> Color32) {
    const SEG: u32 = 72;
    let base = mesh.vertices.len() as u32;
    for &d in ds {
        let col = ring(d);
        for s in 0..SEG {
            let a = s as f32 / SEG as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(pos2(c.x + rx * d * a.cos(), c.y + ry * d * a.sin()), col);
        }
    }
    for j in 0..(ds.len() as u32 - 1) {
        for s in 0..SEG {
            let s1 = (s + 1) % SEG;
            let (a, b) = (base + j * SEG + s, base + j * SEG + s1);
            let (c2, d2) = (a + SEG, b + SEG);
            mesh.add_triangle(a, b, c2);
            mesh.add_triangle(b, d2, c2);
        }
    }
}

pub fn draw(p: &egui::Painter, rect: Rect, accent: Color32) {
    let p = p.with_clip_rect(rect);
    let (w, h) = (rect.width(), rect.height());
    if w < 4.0 || h < 4.0 {
        return;
    }
    let acc = rgb(accent);
    let s = (h / 720.0).clamp(0.45, 2.0); // scale relative to the 720 p design
    let cx = rect.center().x;
    let cy = rect.top() + h * 0.44;

    // ── background: deep blue-black gradient ────────────────────────────────
    let mut bg = Mesh::default();
    let (top, bot) = (Color32::from_rgb(4, 5, 9), Color32::from_rgb(3, 3, 6));
    bg.colored_vertex(rect.left_top(), top);
    bg.colored_vertex(rect.right_top(), top);
    bg.colored_vertex(rect.right_bottom(), bot);
    bg.colored_vertex(rect.left_bottom(), bot);
    bg.add_triangle(0, 1, 2);
    bg.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(bg));

    let mut m = Mesh::default();

    // ── faint grid, fading away from the ribbon ─────────────────────────────
    let fade = |y: f32| (-((y - cy) / (0.5 * h)).powi(2)).exp();
    let step = 80.0 * s;
    let first_x = cx - (((cx - rect.left()) / step).floor()) * step - step * 0.5;
    let mut x = first_x;
    while x < rect.right() {
        // vertical line: a thin quad strip sampled down the height
        let samples = 12;
        let base = m.vertices.len() as u32;
        for i in 0..=samples {
            let y = rect.top() + h * i as f32 / samples as f32;
            let col = light(GRID, 0.028 * fade(y));
            m.colored_vertex(pos2(x - 0.5, y), col);
            m.colored_vertex(pos2(x + 0.5, y), col);
        }
        for i in 0..samples as u32 {
            let a = base + i * 2;
            m.add_triangle(a, a + 1, a + 2);
            m.add_triangle(a + 1, a + 3, a + 2);
        }
        x += step;
    }
    let mut y = cy - (((cy - rect.top()) / step).floor()) * step - step * 0.5;
    while y < rect.bottom() {
        let col = light(GRID, 0.022 * fade(y));
        let r = Rect::from_min_max(pos2(rect.left(), y - 0.5), pos2(rect.right(), y + 0.5));
        let base = m.vertices.len() as u32;
        for c in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
            m.colored_vertex(c, col);
        }
        m.add_triangle(base, base + 1, base + 2);
        m.add_triangle(base, base + 2, base + 3);
        y += step;
    }

    // ── accent glow behind the ribbon ───────────────────────────────────────
    let glow_ds: Vec<f32> = (0..=10).map(|i| i as f32 * 0.16).collect();
    rings(&mut m, pos2(cx, cy), w * 0.55, w * 0.55, &glow_ds, |d| {
        if d >= 1.59 { Color32::TRANSPARENT } else { light(acc, 0.16 * (-2.2 * d * d).exp()) }
    });

    // ── the ribbon: 11 interleaved waves ────────────────────────────────────
    let npts = ((w / 3.0) as usize).clamp(120, 900);
    let lines = 11;
    for k in 0..lines {
        let ph = k as f32 * 0.42;
        let amp = 118.0 * (1.0 - 0.055 * k as f32);
        let mut pts = Vec::with_capacity(npts + 1);
        let mut core = Vec::with_capacity(npts + 1);
        let mut glow = Vec::with_capacity(npts + 1);
        let a = if k == 0 { 1.0 } else { 0.55 * (1.0 - k as f32 / 13.0) };
        for i in 0..=npts {
            let t = i as f32 / npts as f32;
            let u = (t - 0.5) * 2.0;
            let env = (-(u.abs() / 0.78).powf(3.2)).exp();
            let wave = amp * (u * 8.5 + ph * 1.3).sin() * (u * 2.7 + ph).cos()
                + 34.0 * (u * 19.0 + ph * 2.6).sin()
                + 12.0 * (u * 41.0 + ph).sin();
            pts.push(pos2(rect.left() + w * t, cy + env * wave * s));
            let c = ribbon_colour(acc, t);
            core.push(light(c, a));
            glow.push(light(c, a * 0.11));
        }
        strip(&mut m, &pts, 18.0 * s, &glow);
        strip(&mut m, &pts, if k == 0 { 2.3 } else { 1.25 }, &core);
    }

    // ── vignette ────────────────────────────────────────────────────────────
    let vig_ds = [0.25, 0.35, 0.45, 0.55, 0.65, 0.75, 0.85, 1.6];
    rings(&mut m, rect.center(), w / 1.15, h, &vig_ds, |d| {
        let t = ((d - 0.25) / 0.6).clamp(0.0, 1.0);
        Color32::from_black_alpha((0.5 * t.powf(1.6) * 255.0) as u8)
    });

    p.add(egui::Shape::mesh(m));

    // ── hint text in the lower third, clear of the control bar ──────────────
    let ty = (rect.top() + h * 0.80).min(rect.bottom() - 134.0).max(rect.top() + h * 0.55);
    let c = pos2(cx, ty);
    let title = FontId::proportional(20.0);
    p.text(c + vec2(1.0, 1.0), Align2::CENTER_CENTER, "Drop a video or music file to play", title.clone(), Color32::from_black_alpha(160));
    p.text(c, Align2::CENTER_CENTER, "Drop a video or music file to play", title, Color32::from_rgb(235, 238, 245));
    p.text(
        c + vec2(0.0, 26.0),
        Align2::CENTER_CENTER,
        "MP4 · MKV · AVI · MOV · MP3 · FLAC · GIF · and more",
        FontId::proportional(12.5),
        Color32::from_rgb(170, 176, 190),
    );
}
