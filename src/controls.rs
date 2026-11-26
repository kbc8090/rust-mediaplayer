//! The controls over the video: top and bottom bars, their buttons and hand-drawn icons, tooltips,
//! the seek and volume bars, and the volume / seek pop-ups.

use super::*;

// ── Small drawing helpers ─────────────────────────────────────────────────────
pub(crate) fn black(alpha: u8, t: f32) -> Color32 {
    Color32::from_black_alpha((alpha as f32 * t) as u8)
}

/// Darkest shade (fraction of black) behind the top bar, at the top edge
const TOP_SHADE: f32 = 0.30;
/// Darkest shade behind the bottom bar, at the bottom edge
const BOTTOM_SHADE: f32 = 175.0 / 255.0;

/// A shade over the video that fades out along a smooth curve rather than a
/// straight line: `alpha` of black at one edge (the top if `dark_at_top`, else the
/// bottom), easing to nothing at the other. A straight fade stops abruptly, and on
/// bright video the eye sees that stop as the edge of a grey band.
pub(crate) fn eased_shade(p: &egui::Painter, rect: Rect, alpha: f32, dark_at_top: bool) {
    const STEPS: u32 = 16;
    let mut m = egui::Mesh::default();
    for i in 0..=STEPS {
        let s = i as f32 / STEPS as f32; // 0 at the dark edge
        let fade = 1.0 - s * s * (3.0 - 2.0 * s); // smoothstep, reversed
        let c = Color32::from_black_alpha((alpha * fade * 255.0).round() as u8);
        let y = if dark_at_top { rect.top() + s * rect.height() } else { rect.bottom() - s * rect.height() };
        m.colored_vertex(pos2(rect.left(), y), c);
        m.colored_vertex(pos2(rect.right(), y), c);
        if i > 0 {
            let k = i * 2;
            m.add_triangle(k - 2, k - 1, k + 1);
            m.add_triangle(k - 2, k + 1, k);
        }
    }
    p.add(egui::Shape::mesh(m));
}

/// Polyline given as offsets from a centre point
pub(crate) fn poly(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x, c.y + y)).collect();
    p.add(egui::Shape::line(v, Stroke::new(w, col)));
}

/// Click target (one full step wide, so neighbours never overlap) with a faint
/// rounded hover highlight (8 px radius). Draw the icon afterwards.
pub(crate) fn hit(ui: &mut egui::Ui, centre: Pos2, id: &str, tip: &str, t: f32) -> egui::Response {
    hit_at(ui, centre, id, tip, t, true)
}

/// The title of the chapter at `secs` (None if the video has no chapters, or
/// `secs` is before the first one)
pub(crate) fn chapter_at(chapters: &[(f64, String)], secs: f64) -> Option<&str> {
    chapters.iter().rev().find(|(start, _)| *start <= secs + 0.01).map(|(_, t)| t.as_str())
}

/// One line of text cut to `width` with "…"
pub(crate) fn elide(p: &egui::Painter, text: &str, font: FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: width.max(10.0), max_rows: 1, break_anywhere: true, overflow_character: Some('\u{2026}') };
    p.layout_job(job)
}

/// A chapter title in a box like a tooltip, `w` wide at `left`, its bottom at
/// `bottom`: up to two centred lines, wrapped at words, then cut with "…"
fn chapter_box(p: &egui::Painter, title: &str, left: f32, w: f32, bottom: f32, t: f32) {
    let col = TIP_TEXT.gamma_multiply(t);
    let mut job = egui::text::LayoutJob::simple(title.to_owned(), FontId::proportional(12.5), col, (w - 18.0).max(10.0));
    job.wrap = egui::text::TextWrapping { max_width: (w - 18.0).max(10.0), max_rows: 2, break_anywhere: false, overflow_character: Some('\u{2026}') };
    job.halign = egui::Align::Center;
    let g = p.layout_job(job);
    let r = Rect::from_min_max(pos2(left, bottom - g.size().y - 10.0), pos2(left + w, bottom));
    tip_frame(p, r, t);
    // (centred rows are laid out around x = 0)
    p.galley(pos2(r.center().x, r.top() + 5.0), g, col);
}

/// A button that's greyed out: does nothing, but is still a button (so a
/// right-click on it doesn't count as the blank bar)
pub(crate) fn inert(ui: &mut egui::Ui, centre: Pos2, id: &str) {
    // (Sense drags too, like the real buttons: egui finds the topmost widget for
    // clicks and for drags separately, and with clicks only, the bar behind still
    // counted as hovered for drags, which made a right-click here open the menu.)
    let _ = ui.interact(Rect::from_center_size(centre, vec2(BTN_STEP, 34.0)), ui.id().with(id), Sense::click_and_drag());
}

/// `hit` with the tooltip above (`above`) or below the button
pub(crate) fn hit_at(ui: &mut egui::Ui, centre: Pos2, id: &str, tip: &str, t: f32, above: bool) -> egui::Response {
    let r = ui.interact(
        Rect::from_center_size(centre, vec2(BTN_STEP, 34.0)),
        ui.id().with(id),
        // drags starting on a button must not move the window (see the window buttons)
        Sense::click_and_drag(),
    );
    let inside = ui.input(|i| i.pointer.latest_pos()).is_some_and(|p| r.rect.contains(p));
    let pressed = r.is_pointer_button_down_on() && inside;
    if r.hovered() || pressed {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        // hover, and a little brighter while the mouse button is held on it
        let a = if pressed { 13.0 } else { 6.0 };
        ui.painter().rect_filled(
            Rect::from_center_size(centre, vec2(BTN_BOX, BTN_BOX)),
            8.0,
            Color32::from_white_alpha((a * t) as u8),
        );
    }
    hover_tooltip(ui.ctx(), &r, Rect::from_center_size(centre, vec2(BTN_BOX, BTN_BOX)), tip, above);
    r
}

// ── Tooltips ──────────────────────────────────────────────────────────────────
/// Show `text` near `anchor` once the pointer has rested on `resp` for a moment.
/// Placed above (bottom controls) or below (window buttons) from the start, instead
/// of egui's default of trying below the pointer and flipping when it doesn't fit.
pub(crate) fn hover_tooltip(ctx: &egui::Context, resp: &egui::Response, anchor: Rect, text: &str, above: bool) {
    const DELAY: f64 = 0.5;
    let key = resp.id.with("tip_since");
    if !resp.hovered() {
        ctx.data_mut(|d| d.remove::<f64>(key));
        return;
    }
    if resp.is_pointer_button_down_on() {
        // Clicked: no tooltip until the pointer leaves and comes back (like Windows)
        ctx.data_mut(|d| d.insert_temp(key, f64::INFINITY));
        return;
    }
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(key, || now));
    if !since.is_finite() {
        return;
    }
    let age = now - since;
    if age < DELAY {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(DELAY - age));
        return;
    }
    let alpha = ((age - DELAY) / 0.12).min(1.0) as f32; // quick fade-in
    if alpha < 1.0 {
        ctx.request_repaint();
    }
    draw_tooltip(ctx, anchor, text, above, alpha);
}

/// Windows 11-style dark tooltip: near-opaque dark grey, hairline border,
/// small corner radius and a faint shadow.
pub(crate) fn draw_tooltip(ctx: &egui::Context, anchor: Rect, text: &str, above: bool, alpha: f32) {
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("rmp_tooltip")));
    let galley = p.layout_no_wrap(
        text.to_owned(),
        FontId::proportional(12.5),
        Color32::from_rgb(235, 235, 235).gamma_multiply(alpha),
    );
    let pad = vec2(9.0, 5.0);
    let size = galley.size() + pad * 2.0;
    let screen = ctx.screen_rect();
    let x = (anchor.center().x - size.x / 2.0)
        .min(screen.right() - 6.0 - size.x)
        .max(screen.left() + 6.0)
        .round();
    let y = if above { anchor.top() - 6.0 - size.y } else { anchor.bottom() + 6.0 }.round();
    let rect = Rect::from_min_size(pos2(x, y), size);
    tip_frame(&p, rect, alpha);
    p.galley(rect.min + pad, galley, Color32::WHITE);
}

/// The tooltip look, frosted glass when that's on in the menu: button tips, and
/// the seek bar's time and chapter boxes
pub(crate) fn tip_frame(p: &egui::Painter, rect: Rect, alpha: f32) {
    let r = TIP_R;
    glass::shadow(p, rect, r, alpha * 0.6);
    glass::fill(p, rect, r, glass::MENU_BG.gamma_multiply(0.96), alpha);
    let edge = glass::edge(glass::BORDER);
    p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, edge.gamma_multiply(alpha)));
}

/// Corner radius of tooltips and the time / chapter boxes, and of the seek-bar
/// thumbnail so they all match
const TIP_R: f32 = 5.0;
const THUMB_R: f32 = TIP_R;

/// Text colour of tooltips (and the time / chapter boxes)
const TIP_TEXT: Color32 = Color32::from_rgb(235, 235, 235);

// ── Icons (drawn with shapes so they never depend on font coverage) ───────────
pub(crate) const SW: f32 = 1.35;

/// Overall scale of the control-row icons
pub(crate) const K: f32 = 0.92;

/// Like `poly`, with the offsets scaled by the icon scale `K`
pub(crate) fn kpoly(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x * K, c.y + y * K)).collect();
    p.add(egui::Shape::line(v, Stroke::new(w, col)));
}

pub(crate) fn kclosed(p: &egui::Painter, c: Pos2, pts: &[(f32, f32)], col: Color32, w: f32) {
    let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pos2(c.x + x * K, c.y + y * K)).collect();
    p.add(egui::Shape::closed_line(v, Stroke::new(w, col)));
}

pub(crate) fn icon_pause(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_filled(Rect::from_center_size(pos2(c.x - 3.5 * K, c.y), vec2(2.6 * K, 13.0 * K)), 0.5, col);
    p.rect_filled(Rect::from_center_size(pos2(c.x + 3.5 * K, c.y), vec2(2.6 * K, 13.0 * K)), 0.5, col);
}

pub(crate) fn icon_play(p: &egui::Painter, c: Pos2, col: Color32) {
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - 4.5 * K, c.y - 7.5 * K),
            pos2(c.x - 4.5 * K, c.y + 7.5 * K),
            pos2(c.x + 7.5 * K, c.y),
        ],
        col, Stroke::NONE,
    ));
}

pub(crate) fn icon_prev(p: &egui::Painter, c: Pos2, col: Color32) {
    p.line_segment([pos2(c.x - 6.5 * K, c.y - 6.0 * K), pos2(c.x - 6.5 * K, c.y + 6.0 * K)], Stroke::new(SW, col));
    kclosed(p, c, &[(6.5, -6.0), (6.5, 6.0), (-3.0, 0.0)], col, SW);
}

pub(crate) fn icon_next(p: &egui::Painter, c: Pos2, col: Color32) {
    p.line_segment([pos2(c.x + 6.5 * K, c.y - 6.0 * K), pos2(c.x + 6.5 * K, c.y + 6.0 * K)], Stroke::new(SW, col));
    kclosed(p, c, &[(-6.5, -6.0), (-6.5, 6.0), (3.0, 0.0)], col, SW);
}

pub(crate) fn icon_volume(p: &egui::Painter, c: Pos2, col: Color32, level: f64, muted: bool) {
    let c = pos2(c.x - 3.0 * K, c.y);
    kclosed(p, c, &[(-6.0, -3.0), (-3.0, -3.0), (1.5, -7.0), (1.5, 7.0), (-3.0, 3.0), (-6.0, 3.0)], col, SW);
    if muted || level <= 0.0 {
        kpoly(p, c, &[(5.0, -3.5), (10.5, 3.5)], col, SW);
        kpoly(p, c, &[(5.0, 3.5), (10.5, -3.5)], col, SW);
    } else {
        let arc = |r: f32| -> Vec<(f32, f32)> {
            (-4..=4).map(|k| {
                let a = (k as f32) * 11.0_f32.to_radians();
                (1.5 + r * a.cos(), r * a.sin())
            }).collect()
        };
        kpoly(p, c, &arc(5.0), col, SW);
        if level >= 40.0 { kpoly(p, c, &arc(9.0), col, SW); }
    }
}

pub(crate) fn icon_repeat(p: &egui::Painter, c: Pos2, col: Color32) {
    kpoly(p, c, &[(-8.0, 2.0), (-8.0, -2.0), (-5.0, -5.0), (6.0, -5.0)], col, SW);
    kpoly(p, c, &[(3.5, -7.5), (6.0, -5.0), (3.5, -2.5)], col, SW);
    kpoly(p, c, &[(8.0, -2.0), (8.0, 2.0), (5.0, 5.0), (-6.0, 5.0)], col, SW);
    kpoly(p, c, &[(-3.5, 2.5), (-6.0, 5.0), (-3.5, 7.5)], col, SW);
}

/// Playlist: three lines and a small play arrow; `replay`: a small loop arrow in
/// that colour at the corner (the playlist starts again when it ends)
pub(crate) fn icon_playlist(p: &egui::Painter, c: Pos2, col: Color32, replay: Option<Color32>) {
    kpoly(p, c, &[(-8.0, -5.5), (4.0, -5.5)], col, SW);
    kpoly(p, c, &[(-8.0, -0.5), (4.0, -0.5)], col, SW);
    kpoly(p, c, &[(-8.0, 4.5), (-1.0, 4.5)], col, SW);
    let tri = [(2.5, 1.5), (2.5, 8.0), (8.0, 4.75)].map(|(x, y)| pos2(c.x + x * K, c.y + y * K));
    p.add(egui::Shape::convex_polygon(tri.to_vec(), col, Stroke::NONE));
    if let Some(rc) = replay {
        // a small circle arrow at the top right
        let o = pos2(c.x + 8.5 * K, c.y - 6.5 * K);
        let r = 3.2 * K;
        let pts: Vec<Pos2> = (0..=10).map(|i| {
            let a = -0.6 + 4.6 * i as f32 / 10.0;
            pos2(o.x + r * a.cos(), o.y + r * a.sin())
        }).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.3_f32, rc)));
        let a = -0.6 + 4.6;
        let tip = |da: f32, dr: f32| pos2(o.x + (r + dr) * (a + da).cos(), o.y + (r + dr) * (a + da).sin());
        p.add(egui::Shape::convex_polygon(vec![tip(0.55, 0.0), tip(-0.1, 1.9), tip(-0.1, -1.9)], rc, Stroke::NONE));
    }
}

pub(crate) fn icon_captions(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_stroke(Rect::from_center_size(c, vec2(18.0 * K, 13.0 * K)), 2.0, Stroke::new(SW, col));
    kpoly(p, c, &[(-5.0, -1.0), (-1.5, -1.0)], col, SW);
    kpoly(p, c, &[(1.5, -1.0), (5.0, -1.0)], col, SW);
    kpoly(p, c, &[(-5.0, 2.5), (2.0, 2.5)], col, SW);
}

pub(crate) fn icon_compact(p: &egui::Painter, c: Pos2, col: Color32) {
    p.rect_stroke(Rect::from_center_size(c, vec2(17.0 * K, 13.0 * K)), 1.5, Stroke::new(SW, col));
    p.rect_filled(Rect::from_min_size(pos2(c.x + 0.5 * K, c.y - 0.5 * K), vec2(6.0 * K, 4.5 * K)), 0.5, col);
}

pub(crate) fn icon_fullscreen(p: &egui::Painter, c: Pos2, col: Color32) {
    kpoly(p, c, &[(-6.5, 6.5), (6.5, -6.5)], col, SW);
    kpoly(p, c, &[(6.5, -1.5), (6.5, -6.5), (1.5, -6.5)], col, SW);
    kpoly(p, c, &[(-6.5, 1.5), (-6.5, 6.5), (-1.5, 6.5)], col, SW);
}

/// Rotate: a curved arrow turning clockwise over a small rectangle
pub(crate) fn icon_rotate(p: &egui::Painter, c: Pos2, col: Color32) {
    let c = pos2(c.x, c.y - 1.5 * K);
    p.rect_stroke(
        Rect::from_min_max(pos2(c.x - 7.5 * K, c.y), pos2(c.x - 0.5 * K, c.y + 8.0 * K)),
        1.5,
        Stroke::new(SW, col),
    );
    let (ox, oy, r) = (0.5_f32, 1.5_f32, 6.5_f32);
    let arc: Vec<(f32, f32)> = (0..=16)
        .map(|i| {
            let a = (225.0 + 130.0 * i as f32 / 16.0_f32).to_radians();
            (ox + r * a.cos(), oy + r * a.sin())
        })
        .collect();
    kpoly(p, c, &arc, col, SW);
    // arrowhead at the end, pointing down (the way the arc turns)
    let (ex, ey) = *arc.last().unwrap();
    kpoly(p, c, &[(ex - 2.8, ey - 2.2), (ex, ey + 0.6), (ex + 2.6, ey - 2.4)], col, SW);
}

/// Mirror: two triangles facing each other across a dashed line
pub(crate) fn icon_mirror(p: &egui::Painter, c: Pos2, col: Color32) {
    for i in 0..4 {
        let y0 = -8.0 + i as f32 * 4.5;
        kpoly(p, c, &[(0.0, y0), (0.0, y0 + 2.2)], col, SW);
    }
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - 2.5 * K, c.y - 6.0 * K),
            pos2(c.x - 2.5 * K, c.y + 6.0 * K),
            pos2(c.x - 8.5 * K, c.y + 6.0 * K),
        ],
        col,
        Stroke::NONE,
    ));
    kclosed(p, c, &[(2.5, -6.0), (2.5, 6.0), (8.5, 6.0)], col, SW);
}

/// Playback speed: a speedometer, an open dial with a needle pointing up-right
pub(crate) fn icon_speed(p: &egui::Painter, c: Pos2, col: Color32) {
    let c = pos2(c.x, c.y + 1.5 * K); // the dial is open at the bottom: sit it a little lower
    let r = 8.0;
    // 270° arc from bottom-left over the top to bottom-right (screen y points down)
    let dial: Vec<(f32, f32)> = (0..=24)
        .map(|i| {
            let a = (135.0 + 270.0 * i as f32 / 24.0_f32).to_radians();
            (r * a.cos(), r * a.sin())
        })
        .collect();
    kpoly(p, c, &dial, col, SW);
    let n = (-50.0_f32).to_radians();
    kpoly(p, c, &[(0.0, 0.0), (5.5 * n.cos(), 5.5 * n.sin())], col, SW + 0.15);
    adjust::disc(p, c, 1.7 * K, col);
}

/// A bar with fully round ends (a pill), drawn with smooth half circles (egui's own
/// rounded corners are only a few points at this size, and look square)
pub(crate) fn pill(p: &egui::Painter, rect: Rect, color: Color32) {
    let r = rect.height() / 2.0;
    if rect.width() <= 0.0 || r <= 0.0 {
        return;
    }
    if rect.width() <= rect.height() {
        adjust::disc(p, rect.center(), rect.width().min(rect.height()) / 2.0, color);
        return;
    }
    let n = adjust::arc_points(r, p.ctx().pixels_per_point()) / 2;
    let y = rect.center().y;
    let (cl, cr) = (pos2(rect.left() + r, y), pos2(rect.right() - r, y));
    let mut pts = Vec::with_capacity(2 * (n + 1));
    // Clockwise on screen: right end top → bottom, then left end bottom → top
    for i in 0..=n {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / n as f32;
        pts.push(cr + r * vec2(a.cos(), a.sin()));
    }
    for i in 0..=n {
        let a = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / n as f32;
        pts.push(cl + r * vec2(a.cos(), a.sin()));
    }
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// (`gliding`: the knob is on the move by itself, the seek bar during playback. Its
/// x isn't rounded to whole pixels then: rounded, it would hop a pixel at a time
/// while the bar's fill beside it glides.)
pub(crate) fn draw_knob(p: &egui::Painter, ppp: f32, centre: Pos2, hot: bool, accent: Color32, t: f32, gliding: bool) {
    let snap = |v: f32| (v * ppp).round() / ppp;
    let c = pos2(if gliding { centre.x } else { snap(centre.x) }, snap(centre.y));
    let (ring, dot) = if hot { (7.5, 4.6) } else { (6.5, 3.9) };
    adjust::disc(p, pos2(c.x, c.y + 1.0), ring + 0.8, Color32::from_black_alpha((80.0 * t) as u8));
    adjust::disc(p, c, ring, KNOB_RING.gamma_multiply(t));
    adjust::disc(p, c, dot, accent);
}

/// Height of the "most replayed" hill at its highest
const HEAT_H: f32 = 30.0;

/// The "most replayed" hill along the seek bar from `ends.0` to `ends.1`, standing
/// on `base`. `secs_at`: the video time at a point of the bar. `played_to`: where
/// the played part ends (up to there the hill and its outline are in the accent
/// colour).
#[allow(clippy::too_many_arguments)]
fn draw_heatmap(
    p: &egui::Painter,
    h: &crate::online::Heatmap,
    ends: (f32, f32),
    base: f32,
    secs_at: &dyn Fn(f32) -> f64,
    played_to: Option<f32>,
    accent: Color32,
    alpha: f32,
) {
    let (left, right) = ends;
    // (a point every pixel: smooth curves, and still only a thousand or two)
    let steps = (right - left).ceil().max(2.0) as usize;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let x = left + (right - left) * i as f32 / steps as f32;
            pos2(x, base - HEAT_H * h.at(secs_at(x)))
        })
        .collect();
    // The hill: white, fading from 40 % at the graph's full height to 12 % at the
    // bar; under the part already played, the accent colour (50 % to 8 %)
    let shade = |x: f32, y: f32| {
        let f = ((base - y) / HEAT_H).clamp(0.0, 1.0);
        if played_to.is_some_and(|px| x <= px) {
            accent.gamma_multiply((0.08 + 0.42 * f) * alpha)
        } else {
            Color32::from_white_alpha(((0.12 + 0.28 * f) * 255.0 * alpha) as u8)
        }
    };
    // (a mesh has no anti-aliasing of its own, which left the hill's top edge
    // stair-stepped: so it gets its own soft edge, a 1-pixel strip fading out
    // across the curve, like egui gives its shapes)
    let feather = 1.0 / p.ctx().pixels_per_point();
    let mut m = egui::Mesh::default();
    for (i, q) in pts.iter().enumerate() {
        // the curve's outward (upward) direction here
        let (a, b) = (pts[i.saturating_sub(1)], pts[(i + 1).min(pts.len() - 1)]);
        let d = b - a;
        let mut n = vec2(d.y, -d.x).normalized();
        if n.y > 0.0 {
            n = -n;
        }
        let half = n * (feather / 2.0);
        m.colored_vertex(*q - half, shade(q.x, q.y)); // inside edge: full
        m.colored_vertex(pos2(q.x, base), shade(q.x, base));
        m.colored_vertex(*q + half, Color32::TRANSPARENT); // outside edge: none
        if i > 0 {
            let (top0, base0, out0) = (3 * i as u32 - 3, 3 * i as u32 - 2, 3 * i as u32 - 1);
            let (top1, base1, out1) = (3 * i as u32, 3 * i as u32 + 1, 3 * i as u32 + 2);
            m.add_triangle(top0, base0, top1);
            m.add_triangle(base0, top1, base1);
            m.add_triangle(out0, top0, out1);
            m.add_triangle(top0, out1, top1);
        }
    }
    p.add(egui::Shape::mesh(m));
    // Its outline, 1 px: accent over the played part, faint white after it
    let rest_col = Color32::from_white_alpha((0.35 * 255.0 * alpha) as u8);
    let split = played_to.map_or(0, |px| pts.partition_point(|q| q.x < px));
    let mut played: Vec<Pos2> = pts[..split].to_vec();
    let mut rest: Vec<Pos2> = pts[split..].to_vec();
    if let (Some(px), true) = (played_to, split > 0 && split < pts.len()) {
        let (a, b) = (pts[split - 1], pts[split]);
        let mid = a + (b - a) * ((px - a.x) / (b.x - a.x).max(1e-3));
        played.push(mid);
        rest.insert(0, mid);
    }
    if played.len() >= 2 {
        p.add(egui::Shape::line(played, Stroke::new(1.0_f32, accent.gamma_multiply(alpha))));
    }
    if rest.len() >= 2 {
        p.add(egui::Shape::line(rest, Stroke::new(1.0_f32, rest_col)));
    }
}

// ── Title bar (overlaid on the video) ─────────────────────────────────────────
pub(crate) fn draw_top_bar(app: &mut FluentMediaPlayer, ctx: &egui::Context, t: f32) {
    let screen = ctx.screen_rect();
    let bar = Rect::from_min_size(screen.min, vec2(screen.width(), TOP_H));
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    let fullscreen = app.is_fullscreen;
    let title = if app.has_file { app.file_name() } else { "Rust Media Player".to_owned() };
    let white = Color32::WHITE.gamma_multiply(t);

    egui::Area::new(egui::Id::new("top_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(bar.min)
        .show(ctx, |ui| {
            ui.allocate_rect(bar, Sense::hover());
            let p = ui.painter().clone();
            eased_shade(&p, bar, TOP_SHADE * t, true);

            // Fullscreen: the bar covers the top edge of the screen, so it must
            // handle the double-click itself (the video underneath can't see it)
            if fullscreen {
                let bg = ui.interact(bar, ui.id().with("fs_bg"), Sense::click());
                if bg.hovered() {
                    app.blank_hover = true;
                }
                if bg.double_clicked() && !app.is_picture() {
                    app.toggle_fullscreen(ctx);
                }
            }

            // Drag region: move the window; double-click goes fullscreen (not
            // for a picture: there only the full screen button does)
            // (The whole strip, not just its top part: the strip covers the video,
            // so any part of it that doesn't drag is a dead spot.)
            if !fullscreen {
                let drag = ui.interact(bar, ui.id().with("drag"), Sense::click_and_drag());
                if drag.hovered() {
                    app.blank_hover = true;
                }
                let pressed_on_edge = ctx
                    .input(|i| i.pointer.press_origin())
                    .is_some_and(|o| hit_edge(o, screen).is_some());
                if drag.drag_started_by(egui::PointerButton::Primary) && !pressed_on_edge {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                // (fullscreen, like double-clicking the video; the maximise button
                // and dragging to the top of the screen still maximise)
                if drag.double_clicked() && !app.is_picture() {
                    app.toggle_fullscreen(ctx);
                }
            }

            // Left: menu button, then the file name level with it
            let mc = pos2(MENU_X, MENU_Y);
            let menu = hit_at(ui, mc, "menu", "Menu", t, false);
            app.menu_anchor = menu.rect;
            if app.main_menu && app.menu_at.is_none() {
                // Its menu open: the button stays lit, as if pressed (the same as the
                // Playlist button with its panel open; plus the hover's own on top)
                p.rect_filled(Rect::from_center_size(mc, vec2(BTN_BOX, BTN_BOX)), 8.0, Color32::from_white_alpha((13.0 * t) as u8));
            }
            for dy in [-4.5, 0.0, 4.5] {
                p.line_segment([pos2(mc.x - 7.0, mc.y + dy), pos2(mc.x + 7.0, mc.y + dy)], Stroke::new(SW, white));
            }
            // (a right-click on the menu button opens it too, as a left click does)
            if menu.activated() || menu.secondary_clicked() {
                app.main_menu = !app.main_menu;
                app.menu_at = None;
            }

            // (text centred on its line box sits a little low next to the icon)
            let y = MENU_Y - 1.0;
            let mut right_limit = if fullscreen { screen.right() - 20.0 } else { screen.right() - WIN_BTN_W * 3.0 - 10.0 };
            let tx = TITLE_X;

            // A song's format on the right, before the window buttons: an outlined
            // "FLAC" tag in the accent colour, then quiet details (shorter, then
            // left out, when the window is narrow; the title keeps 160 px)
            if let Some((name, details)) = app.song_format() {
                let accent = app.accent.gamma_multiply(t);
                let tag_font = FontId::new(10.5, FontFamily::Name("semibold".into()));
                let det_font = FontId::proportional(12.0);
                let tag = p.layout_no_wrap(name, tag_font, accent);
                let tag_size = tag.size() + vec2(10.0, 3.0);
                let room = right_limit - (tx + 160.0);
                let fits = |d: &String| {
                    let g = p.layout_no_wrap(d.clone(), det_font.clone(), Color32::from_rgb(141, 148, 166).gamma_multiply(t));
                    let w = tag_size.x + if d.is_empty() { 0.0 } else { 8.0 + g.size().x };
                    (w <= room).then_some((g, w))
                };
                if let Some((i, g, w)) = details.iter().enumerate().find_map(|(i, d)| fits(d).map(|(g, w)| (i, g, w))) {
                    // (placed by the widest these details have been for this song,
                    // so a bitrate that changes as it plays doesn't move the tag)
                    let same = app.format_w.1 == i && app.format_w.0 == app.current_file;
                    let w = if same { w.max(app.format_w.2) } else { w };
                    if !same || w > app.format_w.2 {
                        app.format_w = (app.current_file.clone(), i, w);
                    }
                    let left = right_limit - w;
                    let tr = Rect::from_min_size(pos2(left, y - tag_size.y / 2.0), tag_size);
                    p.rect_stroke(tr.shrink(0.5), 4.0, Stroke::new(1.0_f32, accent.gamma_multiply(0.6)));
                    p.galley(tr.center() - tag.size() / 2.0, tag, accent);
                    if g.size().x > 0.0 {
                        p.galley(pos2(tr.right() + 8.0, y - g.size().y / 2.0), g, Color32::from_rgb(141, 148, 166));
                    }
                    right_limit = left - 14.0;
                }
            }
            let clip = p.with_clip_rect(Rect::from_min_max(pos2(tx, 0.0), pos2(right_limit, TOP_H)));
            let font = FontId::new(14.5, FontFamily::Name("semibold".into()));
            clip.text(pos2(tx + 1.0, y + 1.0), Align2::LEFT_CENTER, &title, font.clone(), black(170, t));
            let name = clip.text(pos2(tx, y), Align2::LEFT_CENTER, &title, font, white);
            // A picture: its size after the name ("— 2560 × 1440"), a little quieter
            if let Some((w, h)) = app.picture_size().filter(|_| app.is_picture()) {
                let size = format!("\u{2014}  {w:.0} \u{d7} {h:.0}");
                let at = pos2(name.right() + 8.0, y);
                let small = FontId::proportional(14.0);
                clip.text(at + vec2(1.0, 1.0), Align2::LEFT_CENTER, &size, small.clone(), black(170, t));
                clip.text(at, Align2::LEFT_CENTER, &size, small, Color32::from_rgb(200, 200, 208).gamma_multiply(t));
            }

            // Right: window buttons (not shown in fullscreen)
            if !fullscreen {
                for k in 0..3 {
                    let r = Rect::from_min_size(
                        pos2(screen.right() - WIN_BTN_W * (3 - k) as f32, screen.top()),
                        vec2(WIN_BTN_W, WIN_BTN_H),
                    );
                    // The strip along the window edge resizes instead (see hit_edge), so
                    // the button only reacts inside it (unless maximised)
                    let mut hit_r = r;
                    if !maximized {
                        hit_r.min.y += BTN_EDGE;
                    }
                    // click_and_drag: a button must also "own" drags that start on it,
                    // otherwise egui hands the drag to the draggable title strip behind
                    // it and the window moves
                    let resp = ui.interact(hit_r, ui.id().with(("winbtn", k)), Sense::click_and_drag());
                    let label = match k {
                        0 => "Minimize",
                        1 if maximized => "Restore Down",
                        1 => "Maximize",
                        _ => "Close",
                    };
                    hover_tooltip(ctx, &resp, r, label, false);
                    // Like Windows: lighter while held down (darker for close), and no
                    // highlight while held with the pointer moved off the button
                    let held = resp.is_pointer_button_down_on();
                    let inside = ctx.input(|i| i.pointer.latest_pos()).is_some_and(|pp| hit_r.contains(pp));
                    let pressed = held && inside;
                    if pressed || (resp.hovered() && !held) {
                        // Windows' own colours (the close red differs on 10 and 11)
                        let fill = if k == 2 {
                            let red = if app.caption_font == CaptionFont::Mdl2 {
                                Color32::from_rgb(232, 17, 35)
                            } else {
                                Color32::from_rgb(196, 43, 28)
                            };
                            red.gamma_multiply(if pressed { 0.72 } else { 1.0 } * t)
                        } else {
                            Color32::from_white_alpha((if pressed { 38.0 } else { 22.0 } * t) as u8)
                        };
                        p.rect_filled(r, 0.0, fill);
                    }
                    let c = r.center();
                    let s = Stroke::new(1.1_f32, white);
                    if app.caption_font != CaptionFont::None {
                        // ChromeMinimize / ChromeMaximize / ChromeRestore / ChromeClose
                        let glyph = match k {
                            0 => "\u{E921}",
                            1 if maximized => "\u{E923}",
                            1 => "\u{E922}",
                            _ => "\u{E8BB}",
                        };
                        p.text(c, Align2::CENTER_CENTER, glyph, FontId::new(10.0, FontFamily::Name("caption".into())), white);
                    } else {
                    match k {
                        0 => { p.line_segment([pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)], s); }
                        1 => {
                            if maximized {
                                p.rect_stroke(Rect::from_min_size(pos2(c.x - 5.0, c.y - 2.0), vec2(7.0, 7.0)), 0.0, s);
                                poly(&p, c, &[(-2.0, -2.0), (-2.0, -5.0), (5.0, -5.0), (5.0, 2.0), (2.0, 2.0)], white, 1.1);
                            } else {
                                p.rect_stroke(Rect::from_center_size(c, vec2(10.0, 10.0)), 0.0, s);
                            }
                        }
                        _ => {
                            p.line_segment([pos2(c.x - 5.0, c.y - 5.0), pos2(c.x + 5.0, c.y + 5.0)], s);
                            p.line_segment([pos2(c.x - 5.0, c.y + 5.0), pos2(c.x + 5.0, c.y - 5.0)], s);
                        }
                    }
                    }
                    if resp.activated() {
                        match k {
                            0 => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
                            1 => app.toggle_maximize(ctx),
                            // End the process right here (see `quit`). Hiding the window
                            // and asking egui to close it later doesn't work: a hidden
                            // window gets no more redraws, so the close was never handled.
                            _ => app.quit(),
                        }
                    }
                }
            }
        });
}

// ── Seek bar + control row (overlaid on the video) ────────────────────────────
pub(crate) fn draw_bottom_bar(app: &mut FluentMediaPlayer, ctx: &egui::Context, t: f32) {
    let screen = ctx.screen_rect();
    let bar = Rect::from_min_max(pos2(screen.left(), screen.bottom() - BOTTOM_H), screen.right_bottom());
    let white = Color32::WHITE.gamma_multiply(t);
    let accent = app.accent.gamma_multiply(t);

    egui::Area::new(egui::Id::new("bottom_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(bar.min)
        .show(ctx, |ui| {
            ui.allocate_rect(bar, Sense::hover());
            let p = ui.painter().clone();
            eased_shade(&p, bar, BOTTOM_SHADE * t, false);

            // Empty parts of the bar behave like the video underneath: double-click
            // toggles fullscreen (so the bottom edge of the screen works too), drag
            // moves the window. Registered first, so the seek bar and buttons added
            // after it take precedence.
            let bg = ui.interact(bar, ui.id().with("bar_bg"), Sense::click_and_drag());
            if bg.hovered() {
                app.blank_hover = true;
            }
            if bg.double_clicked() {
                app.toggle_fullscreen(ctx);
            }
            let pressed_on_edge = ctx
                .input(|i| i.pointer.press_origin())
                .is_some_and(|o| hit_edge(o, screen).is_some());
            if bg.drag_started_by(egui::PointerButton::Primary) && !app.is_fullscreen && !pressed_on_edge {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            // ── Seek bar ──────────────────────────────────────────────────────
            let seek_y = screen.bottom() - SEEK_FROM_BOTTOM;
            let sr = Rect::from_min_max(
                pos2(screen.left() + SIDE, seek_y - 10.0),
                pos2(screen.right() - SIDE, seek_y + 10.0),
            );
            let seek = ui.interact(sr, ui.id().with("seek"), Sense::click_and_drag());
            // The knob travels inset by its radius, so at 0 % / 100 % it sits inside
            // the bar's ends (flush with the play / "more" buttons) instead of poking out.
            const KNOB_INSET: f32 = 7.5;
            let (k0, kw) = (sr.left() + KNOB_INSET, sr.width() - 2.0 * KNOB_INSET);
            let frac_at = |x: f32| ((x - k0) / kw).clamp(0.0, 1.0);
            // The stretch of the video the bar stands for: all of it, or (setting up
            // a clip) the part around the clip
            let duration = app.duration;
            let (v0, v1) = app.clip.as_ref().map_or((0.0, duration), |c| c.bar_range(duration));
            let span = (v1 - v0).max(1e-6);
            let to_frac = |t: f64| ((t - v0) / span).clamp(0.0, 1.0) as f32;
            let to_secs = |f: f32| v0 + f as f64 * span;

            // A clip's ends: drag them along the bar (they sit on top of the seek bar)
            let mut end_drag: Option<(ClipEnd, f32)> = None;
            if let Some(c) = app.clip.as_mut() {
                for which in [ClipEnd::Start, ClipEnd::End] {
                    let x = k0 + kw * to_frac(c.time(which));
                    let hr = Rect::from_center_size(pos2(x, seek_y), vec2(14.0, 26.0));
                    let h = ui.interact(hr, ui.id().with(("clip_end", which == ClipEnd::Start)), Sense::click_and_drag());
                    if h.hovered() || h.dragged() {
                        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    if h.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pp) = h.interact_pointer_pos() {
                            c.drag(which, to_secs(frac_at(pp.x)), duration);
                            end_drag = Some((which, to_frac(c.time(which))));
                        }
                    }
                    if h.drag_stopped() {
                        c.dragging = None;
                        c.refit(duration);
                    }
                }
            }

            // A live stream: nothing to seek along, so the bar is simply full
            let live = app.live;
            let dragging = end_drag.is_none()
                && !live
                && (seek.dragged_by(egui::PointerButton::Primary)
                    || (seek.is_pointer_button_down_on() && ctx.input(|i| i.pointer.primary_down())));
            // Seeking with the bar: the press seeks exactly there. Moving while
            // held seeks to the nearest keyframe (quick, so the picture keeps up
            // with the drag), and letting go then seeks exactly to where it ended.
            // Only when the spot changes: this used to seek again every frame
            // while the button was held, even standing still, and each new seek
            // started mpv's over, so the picture waited until the button was let go.
            if dragging {
                if let Some(pos) = seek.interact_pointer_pos() {
                    let f = frac_at(pos.x);
                    let pressed_now = app.drag_frac.is_none();
                    let moved = app.drag_frac != Some(f);
                    app.drag_frac = Some(f);
                    if duration > 0.0 && moved {
                        if pressed_now {
                            app.seek_to(to_secs(f));
                        } else {
                            app.seek_keyframe(to_secs(f));
                            app.drag_rough = true;
                        }
                    }
                }
            } else if let Some(f) = app.drag_frac.take() {
                if std::mem::take(&mut app.drag_rough) && duration > 0.0 {
                    app.seek_to(to_secs(f));
                }
            }
            let frac = if live { 1.0 } else { app.drag_frac.unwrap_or(if duration > 0.0 { to_frac(app.time_pos) } else { 0.0 }) };
            let hot = !live && (seek.hovered() || dragging || end_drag.is_some());

            let track_h = if hot { 4.0 } else { 3.0 };
            let (y0, y1) = (seek_y - track_h / 2.0, seek_y + track_h / 2.0);
            pill(&p, Rect::from_min_max(pos2(sr.left(), y0), pos2(sr.right(), y1)), TRACK.gamma_multiply(t));
            let fill_x = k0 + kw * frac;
            if app.clip.is_none() {
                pill(&p, Rect::from_min_max(pos2(sr.left(), y0), pos2(fill_x, y1)), accent);
            }
            // YouTube's "most replayed" graph: a soft hill over the bar while it's
            // hovered, outlined by a 1 px line (in the accent colour over the part
            // already played). Follows the bar when it's zoomed in around a clip.
            let heat = app.heatmap.clone().filter(|_| duration > 0.0 && !live);
            let heat_a = ctx.animate_bool_with_time(ui.id().with("heat"), hot && heat.is_some(), 0.15);
            if let Some(h) = heat.filter(|_| heat_a > 0.0) {
                let played_to = app.clip.is_none().then_some(fill_x);
                draw_heatmap(&p, &h, (sr.left(), sr.right()), y0 - 3.0, &|x| to_secs(frac_at(x)), played_to, accent, heat_a * t);
            }
            // Chapter marks: small gaps in the bar
            if duration > 0.0 && !live {
                let ppp = ctx.pixels_per_point();
                for &(start, _) in app.chapters.iter() {
                    if start > 0.5 && start < duration - 0.5 && start > v0 && start < v1 {
                        let x = ((k0 + kw * to_frac(start)) * ppp).round() / ppp;
                        p.rect_filled(
                            Rect::from_center_size(pos2(x, seek_y), vec2(2.0, track_h)),
                            0.0, Color32::from_black_alpha((200.0 * t) as u8),
                        );
                    }
                }
            }
            if let Some(c) = &app.clip {
                // The clip: a wider band in the accent colour, a grip at each end,
                // and the playhead as a thin line
                let (xa, xb) = (k0 + kw * to_frac(c.start), k0 + kw * to_frac(c.end));
                p.rect_filled(Rect::from_min_max(pos2(xa, seek_y - 4.5), pos2(xb, seek_y + 4.5)), 2.0, accent);
                for x in [xa, xb] {
                    let g = Rect::from_center_size(pos2(x, seek_y), vec2(8.0, 21.0));
                    p.rect_filled(g.translate(vec2(0.0, 1.0)), 3.0, Color32::from_black_alpha((90.0 * t) as u8));
                    p.rect_filled(g, 3.0, white);
                    p.rect_filled(Rect::from_center_size(g.center(), vec2(2.0, 9.0)), 1.0, Color32::from_black_alpha((80.0 * t) as u8));
                }
                if app.time_pos >= v0 && app.time_pos <= v1 {
                    p.rect_filled(Rect::from_center_size(pos2(fill_x, seek_y), vec2(2.0, 26.0)), 1.0, white);
                }
                // Zoomed in: the stretch's ends, and how to see the whole video
                if c.zoomed_in(duration) {
                    let small = FontId::proportional(11.5);
                    let dim = Color32::from_rgb(210, 210, 210).gamma_multiply(t);
                    p.text(pos2(sr.left(), seek_y - 13.0), Align2::LEFT_BOTTOM, fmt_time(v0), small.clone(), dim);
                    p.text(pos2(sr.right(), seek_y - 13.0), Align2::RIGHT_BOTTOM, fmt_time(v1), small.clone(), dim);
                    if !seek.hovered() && end_drag.is_none() {
                        let g = p.layout_no_wrap("Zoomed in around the clip \u{b7} Z: whole video".to_owned(), small, dim);
                        let r = Rect::from_center_size(pos2(sr.center().x, seek_y - 20.0), g.size() + vec2(16.0, 6.0));
                        p.rect_filled(r, r.height() / 2.0, Color32::from_black_alpha((115.0 * t) as u8));
                        p.galley(r.center() - g.size() / 2.0, g, dim);
                    }
                }
            } else if !live {
                let gliding = app.is_playing && !app.eof && !dragging;
                draw_knob(&p, ctx.pixels_per_point(), pos2(fill_x, seek_y), hot, accent, t, gliding);
            }

            // While it plays, redraw at the screen's refresh rate so the bar glides.
            // (Otherwise the window is only redrawn when mpv has a new video frame:
            // a 24 or 30 fps video moved the bar in 24 / 30 steps a second, on a
            // 60 Hz screen at uneven intervals, which a quickly moving bar shows as
            // jitter. The extra redraws only copy the last video picture: cheap.
            // Vsync holds it to the screen's rate; the 7 ms is a cap in case a
            // driver ignores vsync.)
            if app.is_playing && !app.eof && duration > 0.0 && !live && t > 0.0 {
                ctx.request_repaint_after(std::time::Duration::from_millis(7));
            }

            // Hover / drag time preview (dragging a clip end: that end's time)
            let preview = if let Some((_, f)) = end_drag { Some(f) }
                else if dragging { app.drag_frac }
                else if seek.hovered() && !live { seek.hover_pos().map(|pp| frac_at(pp.x)) }
                else { None };
            if let (Some(f), true) = (preview, duration > 0.0) {
                let secs = to_secs(f);
                let txt = match end_drag {
                    Some((ClipEnd::Start, _)) => format!("Start \u{b7} {}", fmt_tenths(secs)),
                    Some((ClipEnd::End, _)) => format!("End \u{b7} {}", fmt_tenths(secs)),
                    None => fmt_time(secs),
                };
                // (the time in a box like a tooltip)
                let font = FontId::proportional(12.5);
                let w = p.layout_no_wrap(txt.clone(), font.clone(), Color32::WHITE).size().x + 18.0;
                let cx = (k0 + kw * f).clamp(sr.left() + w / 2.0, sr.right() - w / 2.0);
                let r = Rect::from_center_size(pos2(cx, seek_y - 28.0), vec2(w, 24.0));
                tip_frame(&p, r, t);
                p.text(r.center(), Align2::CENTER_CENTER, txt, font, TIP_TEXT.gamma_multiply(t));

                // Thumbnail of that moment, above the time (for a link, once its
                // small preview stream is known)
                let thumbs_ok = !is_url(&app.current_file) || app.link_thumbs.as_deref() == Some(app.current_file.as_str());
                if thumbs_ok {
                    app.thumbs.request(ctx, secs);
                }
                // The chapter there (videos with chapters): its title in a box
                // above the thumbnail, no wider than the thumbnail
                let chapter = chapter_at(&app.chapters, secs).map(str::to_owned);
                let thumb_shown = app.thumbs.aspect().filter(|_| thumbs_ok).is_some();
                if let Some(aspect) = app.thumbs.aspect().filter(|_| thumbs_ok) {
                    let (mut tw, mut th) = (thumbs::THUMB_W, thumbs::THUMB_W / aspect);
                    if th > thumbs::THUMB_MAX_H {
                        th = thumbs::THUMB_MAX_H;
                        tw = th * aspect;
                    }
                    let ppp = ctx.pixels_per_point();
                    let snap = |v: f32| (v * ppp).round() / ppp;
                    let tx = (k0 + kw * f - tw / 2.0).clamp(screen.left() + 8.0, screen.right() - 8.0 - tw);
                    let tr = Rect::from_min_size(pos2(snap(tx), snap(r.top() - 6.0 - th)), vec2(snap(tw), snap(th)));
                    let tp = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("thumb")));
                    for (e, a) in [(1.0, 60.0), (2.5, 30.0), (4.0, 14.0)] {
                        tp.rect_filled(tr.expand(e), THUMB_R + e, Color32::from_black_alpha((a * t) as u8));
                    }
                    app.thumbs.paint(&tp, tr, THUMB_R);
                    tp.rect_stroke(tr.expand(0.5), THUMB_R + 0.5, Stroke::new(1.0_f32, { let a = (70.0 * t) as u8; Color32::from_rgba_premultiplied(a, a, a, a) }));
                    if let Some(title) = &chapter {
                        chapter_box(&tp, title, tr.left(), tr.width(), tr.top() - 5.0, t);
                    }
                }
                // (no thumbnail: the box goes above the time instead)
                if let (Some(title), false) = (&chapter, thumb_shown) {
                    let w = p.layout_no_wrap(title.clone(), FontId::proportional(12.5), white).size().x + 16.0;
                    let bw = w.min(thumbs::THUMB_W);
                    let bx = (k0 + kw * f - bw / 2.0).clamp(screen.left() + 8.0, screen.right() - 8.0 - bw);
                    chapter_box(&p, title, bx, bw, r.top() - 5.0, t);
                }
            }

            // ── Button row ────────────────────────────────────────────────────
            let row_y = screen.bottom() - ROW_FROM_BOTTOM;
            let left = screen.left();
            let right = screen.right();

            // (a live stream: how long it's been playing, then a red dot and "Live")
            let time_text = if live { fmt_time(app.time_pos) } else { format!("{} / {}", fmt_time(app.time_pos), fmt_time(app.duration)) };
            let time_font = FontId::new(14.5, FontFamily::Name("semibold".into()));
            let clock_w = p.layout_no_wrap(time_text.clone(), time_font.clone(), white).size().x;
            const LIVE_GAP: f32 = 12.0;
            const LIVE_DOT: f32 = 4.0;
            let live_g = live.then(|| p.layout_no_wrap("Live".to_owned(), time_font.clone(), white));
            let time_w = clock_w + live_g.as_ref().map_or(0.0, |g| LIVE_GAP + LIVE_DOT * 2.0 + 6.0 + g.size().x);
            let lay = row_layout(left, right, time_w, app.is_music());

            // Play / pause
            let c = pos2(lay.play, row_y);
            if hit(ui, c, "play", if app.is_playing { "Pause (Space)" } else { "Play (Space)" }, t).activated() {
                app.toggle_play_pause();
            }
            if app.is_playing { icon_pause(&p, c, white) } else { icon_play(&p, c, white) }

            // Previous / next file in the same folder (dimmed when there isn't one)
            let dim = white.gamma_multiply(0.4);
            let thing = app.item_noun();
            let c = pos2(lay.prev, row_y);
            if app.has_prev() {
                if hit(ui, c, "prev", &format!("Previous {thing} (P)"), t).activated() { app.play_adjacent(-1); }
            } else {
                inert(ui, c, "prev");
            }
            icon_prev(&p, c, if app.has_prev() { white } else { dim });

            let c = pos2(lay.next, row_y);
            if app.has_next() {
                if hit(ui, c, "next", &format!("Next {thing} (N)"), t).activated() { app.play_adjacent(1); }
            } else {
                inert(ui, c, "next");
            }
            icon_next(&p, c, if app.has_next() { white } else { dim });

            // Time
            p.text(pos2(lay.time_x, row_y - 1.0), Align2::LEFT_CENTER, time_text, time_font, white);
            if let Some(g) = live_g {
                let dot = pos2(lay.time_x + clock_w + LIVE_GAP + LIVE_DOT, row_y);
                p.circle_filled(dot, LIVE_DOT, Color32::from_rgb(232, 17, 35).gamma_multiply(t));
                p.galley(pos2(dot.x + LIVE_DOT + 6.0, row_y - 1.0 - g.size().y / 2.0), g, white);
            }
            // The chapter playing (videos with chapters): "12:41 / 33:07 | Title",
            // cut short to the room before the buttons on the right
            if let Some(title) = chapter_at(&app.chapters, app.time_pos) {
                let x = lay.time_x + time_w + 10.0;
                let room = lay.mute - BTN_BOX / 2.0 - 14.0 - x;
                if room > 60.0 {
                    p.text(pos2(x, row_y - 1.0), Align2::LEFT_CENTER, "|", FontId::proportional(14.0), Color32::from_white_alpha((90.0 * t) as u8));
                    let g = elide(&p, title, FontId::proportional(14.0), white, room - 14.0);
                    p.galley(pos2(x + 12.0, row_y - 1.0 - g.size().y / 2.0), g, white);
                }
            }

            // Volume icon (click = mute) + slider (the slider is left out when the
            // window is too narrow for it; the wheel and arrow keys still work)
            let c = pos2(lay.mute, row_y);
            if hit(ui, c, "mute", if app.muted { "Unmute (M)" } else { "Mute (M)" }, t).activated() {
                let m = !app.muted;
                app.set_mute(m);
            }
            icon_volume(&p, c, white, app.volume, app.muted);

            if let Some(vl) = lay.vol_x {
            let vw = VOL_W;
            // (its grab area stops where the volume button's starts)
            let vr = Rect::from_min_max(pos2(vl - VOL_GAP, row_y - 12.0), pos2(vl + vw + 8.0, row_y + 12.0));
            let vresp = ui.interact(vr, ui.id().with("vol"), Sense::click_and_drag());
            if vresp.is_pointer_button_down_on() || vresp.dragged() {
                if let Some(pos) = vresp.interact_pointer_pos() {
                    let mut v = ((pos.x - vl) / vw).clamp(0.0, 1.0) as f64 * VOL_MAX;
                    // Gentle snap to 100 % so it's easy to land exactly on it
                    if (v - 100.0).abs() < 2.5 { v = 100.0; }
                    app.set_volume(v.round());
                    if app.muted { app.set_mute(false); }
                    app.volume_osd = Some(std::time::Instant::now());
                }
            }
            let vf = (app.volume / VOL_MAX).clamp(0.0, 1.0) as f32;
            let vhot = vresp.hovered() || vresp.dragged();
            pill(&p, Rect::from_min_max(pos2(vl, row_y - 1.5), pos2(vl + vw, row_y + 1.5)), VOL_TRACK.gamma_multiply(t));
            // Small tick marking 100 % (everything right of it is boost)
            let x100 = vl + vw * (100.0 / VOL_MAX) as f32;
            p.line_segment(
                [pos2(x100, row_y - 4.0), pos2(x100, row_y + 4.0)],
                Stroke::new(1.0_f32, VOL_TRACK.gamma_multiply(t * 0.8)),
            );
            pill(&p, Rect::from_min_max(pos2(vl, row_y - 1.5), pos2(vl + vw * vf, row_y + 1.5)), accent);
            draw_knob(&p, ctx.pixels_per_point(), pos2(vl + vw * vf, row_y), vhot, accent, t, false);
            }

            // Right-hand buttons
            let tint = |on: bool| if on { accent } else { white };

            // Rotate / mirror (left out when the window is too narrow)
            if let (Some(rx), Some(mx)) = (lay.rotate, lay.mirror) {
                let c = pos2(rx, row_y);
                if hit(ui, c, "rotate", "Rotate 90\u{b0} (R)", t).activated() { app.rotate_90(); }
                icon_rotate(&p, c, tint(app.rotation != 0));

                let c = pos2(mx, row_y);
                if hit(ui, c, "mirror", "Mirror", t).activated() { app.toggle_mirror(); }
                icon_mirror(&p, c, tint(app.mirror));
            }

            // Repeat; while a playlist plays (which always plays on), the Playlist
            // button instead: click for the list, right-click to replay it at the end
            let c = pos2(lay.repeat, row_y);
            if app.playlist.is_some() {
                let n = app.folder_files.len();
                let noun = app.playlist_noun();
                let tip = format!(
                    "Playlist: {n} {noun} (L)\nRight-click: replay it when it ends ({})",
                    if app.playlist_replay { "on" } else { "off" }
                );
                let r = hit(ui, c, "repeat", &tip, t);
                // Panel open: the button stays lit, its highlight on as if pressed
                // (plus the hover's own when the mouse is on it)
                if app.adjust_ui.playlist_open {
                    p.rect_filled(Rect::from_center_size(c, vec2(BTN_BOX, BTN_BOX)), 8.0, Color32::from_white_alpha((13.0 * t) as u8));
                }
                if r.activated() {
                    app.toggle_playlist_panel();
                }
                if r.secondary_clicked() {
                    app.playlist_replay = !app.playlist_replay;
                }
                icon_playlist(&p, c, tint(app.adjust_ui.playlist_open), app.playlist_replay.then_some(accent));
            } else {
                if hit(ui, c, "repeat", "Repeat this video or song", t).activated() { app.toggle_repeat(); }
                icon_repeat(&p, c, tint(app.repeat));
            }

            if let Some(cx) = lay.captions {
                let c = pos2(cx, row_y);
                if hit(ui, c, "captions", "Cycle subtitles", t).activated() { app.cycle_captions(); }
                icon_captions(&p, c, white);
            }

            let c = pos2(lay.ontop, row_y);
            if hit(ui, c, "ontop", "Always on top", t).activated() { app.toggle_on_top(ctx); }
            icon_compact(&p, c, tint(app.on_top));

            let c = pos2(lay.fullscreen, row_y);
            if hit(ui, c, "fs", "Full screen (Enter)", t).activated() { app.toggle_fullscreen(ctx); }
            icon_fullscreen(&p, c, white);

            // "More" → playback speed menu
            let c = pos2(lay.speed, row_y);
            let more = hit(ui, c, "more", "Playback speed", t);
            icon_speed(&p, c, white);
            app.speed_anchor = more.rect;
            if more.activated() {
                app.speed_menu = !app.speed_menu;
            }
        });
}

// ── Control-row layout ────────────────────────────────────────────────────────
/// Horizontal centres of the control-row items for a window spanning left..right.
/// The first button's highlight starts exactly where the seek bar starts and the last
/// one ends where the seek bar ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RowLayout {
    pub(crate) play: f32,
    pub(crate) prev: f32,
    pub(crate) next: f32,
    pub(crate) time_x: f32,
    pub(crate) mute: f32,
    /// Left end of the volume slider track, or None when there's no room for it
    pub(crate) vol_x: Option<f32>,
    /// Rotate / mirror buttons, None when there's no room for them
    pub(crate) rotate: Option<f32>,
    pub(crate) mirror: Option<f32>,
    pub(crate) repeat: f32,
    /// None for songs (no picture to turn, no subtitles), as rotate / mirror
    pub(crate) captions: Option<f32>,
    pub(crate) ontop: f32,
    pub(crate) speed: f32,
    /// The last button, flush with the seek bar's right end
    pub(crate) fullscreen: f32,
}

/// `music`: a song is playing, so rotate, mirror and subtitles are left out
pub(crate) fn row_layout(left: f32, right: f32, time_w: f32, music: bool) -> RowLayout {
    let half = BTN_BOX / 2.0;
    let play = left + SIDE + half;
    let prev = play + BTN_STEP;
    let next = prev + BTN_STEP;
    let time_x = next + half + 10.0;
    let time_end = time_x + time_w;

    let fullscreen = right - SIDE - half;
    let speed = fullscreen - BTN_STEP;
    let ontop = speed - BTN_STEP;
    let captions = ontop - BTN_STEP;
    let repeat = if music { ontop - BTN_STEP } else { captions - BTN_STEP };
    if music {
        let (mute, vol_x) = {
            let vol_x = repeat - half - 12.0 - VOL_W;
            let m = vol_x - VOL_GAP - half;
            if m - half >= time_end + 14.0 { (m, Some(vol_x)) } else { (repeat - BTN_STEP, None) }
        };
        return RowLayout { play, prev, next, time_x, mute, vol_x, rotate: None, mirror: None, repeat, captions: None, ontop, speed, fullscreen };
    }

    // Then, right to left: mirror, rotate, the volume slider, the volume icon.
    // When the window is too narrow the slider goes first (the wheel and arrow
    // keys still set the volume), then rotate / mirror.
    let mirror = repeat - BTN_STEP;
    let rotate = mirror - BTN_STEP;
    let fits = |mute: f32| mute - half >= time_end + 14.0;
    let with_slider = |first: f32| {
        let vol_x = first - half - 12.0 - VOL_W;
        (vol_x - VOL_GAP - half, vol_x) // just room for the knob at 0 %
    };
    let (mute, vol_x, rotate, mirror) = {
        let (m, v) = with_slider(rotate);
        if fits(m) {
            (m, Some(v), Some(rotate), Some(mirror))
        } else if fits(rotate - BTN_STEP) {
            (rotate - BTN_STEP, None, Some(rotate), Some(mirror))
        } else {
            (repeat - BTN_STEP, None, None, None)
        }
    };
    RowLayout { play, prev, next, time_x, mute, vol_x, rotate, mirror, repeat, captions: Some(captions), ontop, speed, fullscreen }
}

// ── Pop-ups ───────────────────────────────────────────────────────────────────
/// Pill in the top left: icon, value, and a slim bar showing `frac`
pub(crate) fn draw_osd(
    ctx: &egui::Context,
    icon: &dyn Fn(&egui::Painter, Pos2, Color32),
    label: &str,
    frac: f32,
    accent: Color32,
    alpha: f32,
) {
    let screen = ctx.screen_rect();
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("osd")));
    let font = FontId::new(17.0, FontFamily::Name("semibold".into()));
    let label_w = p.layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x;
    // Top left, just below the menu button and lined up with it
    let size = vec2((label_w + 66.0).max(116.0).round(), 42.0);
    let rect = Rect::from_min_size(pos2(screen.left() + OSD_LEFT, screen.top() + OSD_TOP), size);
    p.rect_filled(rect, 8.0, Color32::from_rgba_unmultiplied(24, 24, 24, (225.0 * alpha) as u8));

    let white = Color32::WHITE.gamma_multiply(alpha);
    icon(&p, pos2(rect.left() + 26.0, rect.center().y - 3.0), white);
    p.text(pos2(rect.right() - 16.0, rect.center().y - 3.0), Align2::RIGHT_CENTER, label, font, white);

    // 4 px tall and lined up with whole screen pixels, so its round ends show
    let ppp = ctx.pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let bar = Rect::from_min_max(
        pos2(snap(rect.left() + 14.0), snap(rect.bottom() - 10.0)),
        pos2(snap(rect.right() - 14.0), snap(rect.bottom() - 6.0)),
    );
    pill(&p, bar, VOL_TRACK.gamma_multiply(alpha * 0.6));
    pill(
        &p,
        Rect::from_min_max(bar.min, pos2(bar.left() + bar.width() * frac.clamp(0.0, 1.0), bar.max.y)),
        accent.gamma_multiply(alpha),
    );
}

/// "🔊 55 %" with the volume level
pub(crate) fn draw_volume_osd(ctx: &egui::Context, volume: f64, muted: bool, accent: Color32, alpha: f32) {
    let label = if muted { "Muted".to_owned() } else { format!("{}%", volume.round() as i32) };
    let frac = if muted { 0.0 } else { (volume / VOL_MAX) as f32 };
    draw_osd(ctx, &|p, c, col| icon_volume(p, c, col, volume, muted), &label, frac, accent, alpha);
}

/// "⏩ +5 sec" after the arrow keys; the bar shows the position in the video
pub(crate) fn draw_seek_osd(ctx: &egui::Context, kind: SeekOsd, target: f64, duration: f64, accent: Color32, alpha: f32) {
    let (forward, label) = match kind {
        SeekOsd::Step(secs) => {
            let sign = if secs < 0.0 { "\u{2212}" } else { "+" };
            let a = secs.abs();
            (secs >= 0.0, if a < 60.0 { format!("{sign}{} sec", a.round()) } else { format!("{sign}{}", fmt_time(a)) })
        }
    };
    let frac = if duration > 0.0 { (target / duration) as f32 } else { 0.0 };
    draw_osd(ctx, &|p, c, col| icon_seek(p, c, col, forward), &label, frac, accent, alpha);
}

/// Fast-forward (two triangles), mirrored for rewind
pub(crate) fn icon_seek(p: &egui::Painter, c: Pos2, col: Color32, forward: bool) {
    let d = if forward { 1.0 } else { -1.0 };
    for x0 in [-7.5_f32, 0.0] {
        p.add(egui::Shape::convex_polygon(
            vec![
                pos2(c.x + d * x0 * K, c.y - 6.0 * K),
                pos2(c.x + d * (x0 + 7.5) * K, c.y),
                pos2(c.x + d * x0 * K, c.y + 6.0 * K),
            ],
            col,
            Stroke::NONE,
        ));
    }
}

// ── Dropping files: what's coming ─────────────────────────────────────────────
/// What's being dragged over the window, worked out from the file names
/// (Windows says which files they are while they're still being dragged)
#[derive(Clone)]
pub(crate) struct DropPreview {
    icon: DropIcon,
    /// In big letters: "FLAC", "MP4", "8 SONGS"…
    big: String,
    /// Under it: the file name, or how many of each format
    detail: String,
    /// "Drop to play", "Drop to play as a playlist", "Can't play this file"
    action: &'static str,
    playable: bool,
    /// An archive with several kinds in it: what to play is asked once it's dropped
    choose: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum DropIcon {
    Music,
    Video,
    Picture,
    Playlist,
    Unknown,
}

/// (`inside`: what's in it, when it's one archive)
fn drop_preview(paths: &[String], inside: Option<&Inside>) -> Option<DropPreview> {
    if paths.is_empty() {
        return None;
    }
    let ext = |p: &str| Path::new(p).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    let name = |p: &str| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // An archive: what's in it ("ZIP" while it's being looked into)
    if let (Some(inside), [archive]) = (inside, paths) {
        let (big, detail) = (ext(archive), name(archive));
        return Some(match inside {
            Inside::Looking => DropPreview { icon: DropIcon::Playlist, big, detail, action: "Looking inside\u{2026}", playable: true, choose: false },
            Inside::Media(files) if files.len() == 1 => {
                let icon = match media_kind(&files[0]) {
                    Some(MediaKind::Music) => DropIcon::Music,
                    Some(MediaKind::Image) => DropIcon::Picture,
                    _ => DropIcon::Video,
                };
                DropPreview { icon, big: ext(&files[0]), detail: name(&files[0]), action: "Drop to play", playable: true, choose: false }
            }
            Inside::Media(files) => {
                let noun = match media_kind(&files[0]) {
                    Some(MediaKind::Music) => "SONGS",
                    Some(MediaKind::Image) => "PICTURES",
                    _ => "VIDEOS",
                };
                DropPreview { icon: DropIcon::Playlist, big: format!("{} {noun}", files.len()), detail, action: "Drop to play as a playlist", playable: true, choose: false }
            }
            Inside::Mixed(kinds) => {
                let n: usize = kinds.iter().map(|(_, f)| f.len()).sum();
                DropPreview { icon: DropIcon::Playlist, big: format!("{n} FILES"), detail, action: "Drop, then choose what to play", playable: true, choose: true }
            }
            Inside::Nothing => DropPreview { icon: DropIcon::Unknown, big, detail, action: "Nothing to play inside", playable: false, choose: false },
            Inside::Unreadable => DropPreview { icon: DropIcon::Unknown, big, detail, action: "Can't open this archive", playable: false, choose: false },
        });
    }
    let media: Vec<&String> = paths.iter().filter(|p| is_media_file(p)).collect();
    // One thing to play: that file (or a playlist file on its own)
    let single = match (paths.len(), media.len()) {
        (_, 1) => Some(media[0].as_str()),
        (1, 0) => Some(paths[0].as_str()),
        _ => None,
    };
    if let Some(p) = single {
        let (icon, playable) = match media_kind(p) {
            Some(MediaKind::Music) => (DropIcon::Music, true),
            Some(MediaKind::Video) => (DropIcon::Video, true),
            Some(MediaKind::Image) => (DropIcon::Picture, true),
            None if is_playlist_file(p) => (DropIcon::Playlist, true),
            None => (DropIcon::Unknown, false),
        };
        let big = match ext(p) {
            e if e.is_empty() && Path::new(p).is_dir() => "FOLDER".to_owned(),
            e if e.is_empty() => "?".to_owned(),
            e => e,
        };
        let action = if playable { "Drop to play" } else { "Can't play this file" };
        return Some(DropPreview { icon, big, detail: name(p), action, playable, choose: false });
    }
    if media.is_empty() {
        return Some(DropPreview { icon: DropIcon::Unknown, big: format!("{} FILES", paths.len()), detail: String::new(), action: "Can't play these files", playable: false, choose: false });
    }
    // Several: a playlist of them ("8 SONGS", or videos, or just files when mixed)
    let n = media.len();
    let kinds: Vec<Option<MediaKind>> = media.iter().map(|p| media_kind(p)).collect();
    let all = |k: MediaKind| kinds.iter().all(|x| *x == Some(k));
    let noun = if all(MediaKind::Music) { "SONGS" } else if all(MediaKind::Video) { "VIDEOS" } else if all(MediaKind::Image) { "PICTURES" } else { "FILES" };
    // "FLAC ×6 · MP3 ×2", most first
    let mut counts: Vec<(String, usize)> = Vec::new();
    for p in &media {
        let e = ext(p);
        match counts.iter_mut().find(|(x, _)| *x == e) {
            Some((_, c)) => *c += 1,
            None => counts.push((e, 1)),
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    let detail = counts.iter().take(4).map(|(e, c)| format!("{e} \u{d7}{c}")).collect::<Vec<_>>().join(" \u{b7} ");
    Some(DropPreview { icon: DropIcon::Playlist, big: format!("{n} {noun}"), detail, action: "Drop to play as a playlist", playable: true, choose: false })
}

/// Text whose colour runs across it along `colour(0…1)` (each glyph's
/// vertices recoloured by where they are)
fn gradient_text(p: &egui::Painter, centre: Pos2, text: &str, font: FontId, colour: &dyn Fn(f32) -> Color32) {
    let g = p.layout_no_wrap(text.to_owned(), font, Color32::WHITE);
    let ppp = p.ctx().pixels_per_point();
    let origin = ((centre - g.size() / 2.0) * ppp).round() / ppp;
    let w = g.size().x.max(1.0);
    // (a galley's vertices are relative to it, their texture coordinates in
    // font-atlas pixels: the same conversion egui makes when it draws text)
    let [tw, th] = p.ctx().fonts(|f| f.font_image_size());
    let uv_scale = vec2(1.0 / tw as f32, 1.0 / th as f32);
    for row in &g.rows {
        let mut m = row.visuals.mesh.clone();
        for v in &mut m.vertices {
            v.pos = origin + v.pos.to_vec2();
            v.uv = (v.uv.to_vec2() * uv_scale).to_pos2();
            let f = ((v.pos.x - origin.x) / w).clamp(0.0, 1.0);
            v.color = colour(f);
        }
        p.add(egui::Shape::mesh(m));
    }
}

/// The kind-of-file icon, drawn with lines `size` across; each piece coloured
/// by where it is across the icon
fn drop_icon(p: &egui::Painter, c: Pos2, size: f32, icon: DropIcon, colour: &dyn Fn(f32) -> Color32) {
    let k = size / 24.0;
    let w = (1.6 * k).max(1.5);
    let at = |x: f32, y: f32| pos2(c.x + (x - 12.0) * k, c.y + (y - 12.0) * k);
    let col_at = |x: f32| colour((x / 24.0).clamp(0.0, 1.0));
    let line = |pts: &[(f32, f32)]| {
        for s in pts.windows(2) {
            let mid = (s[0].0 + s[1].0) / 2.0;
            p.line_segment([at(s[0].0, s[0].1), at(s[1].0, s[1].1)], Stroke::new(w, col_at(mid)));
        }
    };
    let circle = |cx: f32, cy: f32, r: f32| {
        let pts: Vec<(f32, f32)> = (0..=32).map(|i| {
            let a = i as f32 / 32.0 * std::f32::consts::TAU;
            (cx + r * a.cos(), cy + r * a.sin())
        }).collect();
        line(&pts);
    };
    let rrect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let mut pts = Vec::new();
        for (cx, cy, a0) in [(x1 - r, y0 + r, -90.0_f32), (x1 - r, y1 - r, 0.0), (x0 + r, y1 - r, 90.0), (x0 + r, y0 + r, 180.0)] {
            for i in 0..=6 {
                let a = (a0 + i as f32 * 15.0).to_radians();
                pts.push((cx + r * a.cos(), cy + r * a.sin()));
            }
        }
        pts.push(pts[0]);
        line(&pts);
    };
    match icon {
        DropIcon::Music => {
            line(&[(9.0, 18.0), (9.0, 6.0), (19.0, 4.0), (19.0, 16.0)]);
            circle(6.5, 18.0, 2.5);
            circle(16.5, 16.0, 2.5);
        }
        DropIcon::Video => {
            rrect(3.0, 6.0, 16.0, 18.0, 2.5);
            line(&[(16.0, 10.5), (21.0, 7.5), (21.0, 16.5), (16.0, 13.5)]);
        }
        DropIcon::Picture => {
            rrect(3.0, 5.0, 21.0, 19.0, 2.5);
            line(&[(3.5, 17.0), (9.0, 11.5), (13.0, 15.5), (15.5, 13.0), (20.5, 18.0)]);
            circle(15.5, 9.0, 1.6);
        }
        DropIcon::Playlist => {
            line(&[(4.0, 6.0), (16.0, 6.0)]);
            line(&[(4.0, 11.0), (16.0, 11.0)]);
            line(&[(4.0, 16.0), (11.0, 16.0)]);
            let tri = vec![at(15.0, 13.5), at(15.0, 19.5), at(20.0, 16.5)];
            p.add(egui::Shape::convex_polygon(tri, col_at(17.5), Stroke::NONE));
        }
        DropIcon::Unknown => {
            circle(12.0, 12.0, 8.5);
            line(&[(6.0, 6.0), (18.0, 18.0)]);
        }
    }
}

/// While files are dragged over the window: between the title bar and the
/// controls, the video blurs inside an accent-coloured frame (grey for
/// something it can't play), with an
/// icon for the kind of file, its format in big letters, and what dropping it
/// will do
pub(crate) fn draw_drop_overlay(app: &mut FluentMediaPlayer, ctx: &egui::Context) {
    let paths: Vec<String> = ctx.input(|i| {
        i.raw.hovered_files.iter().filter_map(|f| f.path.as_ref().map(|p| p.to_string_lossy().into_owned())).collect()
    });
    // A video open (playing or paused) and more of the same kind dragged over
    // it (MP4 over MP4): nothing at all, it just plays when dropped. A
    // different kind (an MKV over an MP4) gets the overlay.
    if quiet_drop(app, &paths) {
        app.drop_seen = None;
        return;
    }
    let inside = match paths.as_slice() {
        [p] if is_archive_file(p) => Some(app.peek_archive(p)),
        _ => None,
    };
    if let Some(d) = drop_preview(&paths, inside.as_ref()) {
        paint_drop(ctx, app.accent, &d, None);
        app.drop_seen = Some(d);
        return;
    }
    // Let go: a short send-off (a third of a second) for songs, pictures and
    // playlists (a video just starts). Taken: the frame opens out a little and
    // everything fades; refused: it shakes its head, then fades. (Dragged away
    // again: it just goes.)
    if let Some(d) = app.drop_seen.take() {
        // (nor for an archive that asks what to play: the question takes its place)
        if ctx.input(|i| !i.raw.dropped_files.is_empty()) && d.icon != DropIcon::Video && !d.choose {
            app.drop_anim = Some((d, std::time::Instant::now()));
        }
    }
    if let Some((d, t0)) = &app.drop_anim {
        let k = t0.elapsed().as_secs_f32() / DROP_ANIM_SECS;
        if k >= 1.0 {
            app.drop_anim = None;
        } else {
            paint_drop(ctx, app.accent, d, Some(k));
            ctx.request_repaint();
        }
    }
}

/// Videos of the same kind as the video that's open (playing or paused) being
/// dragged over it: no overlay (they just play when dropped)
pub(crate) fn quiet_drop(app: &FluentMediaPlayer, paths: &[String]) -> bool {
    let video_open = app.has_file && (app.video_kind == VideoKind::Video || matches!(media_kind(&app.current_file), Some(MediaKind::Video)));
    let ext = |p: &str| Path::new(p).extension().map(|e| e.to_string_lossy().to_lowercase());
    let open_ext = ext(&app.current_file);
    let mut media = paths.iter().filter(|p| is_media_file(p)).peekable();
    video_open
        && open_ext.is_some()
        && media.peek().is_some()
        && media.all(|p| matches!(media_kind(p), Some(MediaKind::Video)) && ext(p) == open_ext)
}

/// How long the send-off after a drop takes
const DROP_ANIM_SECS: f32 = 0.32;

/// The drop overlay itself. `after`: how far through the send-off after the
/// drop (0…1), None while still dragging.
fn paint_drop(ctx: &egui::Context, accent: Color32, d: &DropPreview, after: Option<f32>) {
    let screen = ctx.screen_rect();
    let area = Rect::from_min_max(
        pos2(screen.left(), screen.top() + WIN_BTN_H + 1.0),
        pos2(screen.right(), screen.bottom() - SEEK_FROM_BOTTOM - 12.0),
    );
    if area.height() < 60.0 {
        return;
    }
    let k = after.unwrap_or(0.0).clamp(0.0, 1.0);
    let out = 1.0 - (1.0 - k).powi(3); // (eased)
    let alpha = 1.0 - out;
    // Taken: opens out a little; refused: a quick shake that dies down
    let (grow, shake) = if d.playable {
        (14.0 * out, 0.0)
    } else {
        (0.0, (k * std::f32::consts::PI * 6.0).sin() * 9.0 * (1.0 - k))
    };
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drop_overlay")));
    let acc = crate::splash::rgb(accent);
    let grey = move |_: f32| Color32::from_rgb(128, 128, 136).gamma_multiply(alpha);
    // (the start screen's teal → accent → violet, but mostly the accent: a
    // gentle shift across the letters rather than the full ribbon)
    let ribbon = move |f: f32| {
        let [r, g, b] = crate::splash::mix(acc, crate::splash::ribbon_colour(acc, f), 0.4);
        Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8).gamma_multiply(alpha)
    };
    let colour: &dyn Fn(f32) -> Color32 = if d.playable { &ribbon } else { &grey };

    // Inside a rounded frame 10 px in from the window's edges: the video blurred
    // (the frosted glass the menus use; a dark veil without it), outlined in
    // the accent colour (grey for something it can't play)
    let fr = area.shrink(10.0).expand(grow).translate(vec2(shake, 0.0));
    let r = 16.0;
    crate::glass::fill(&p, fr, r, Color32::from_black_alpha(170), alpha);
    let edge = if d.playable { accent } else { Color32::from_rgb(128, 128, 136) };
    p.rect_stroke(fr.shrink(1.0), r - 1.0, Stroke::new(2.0_f32, edge.gamma_multiply(alpha)));

    // The icon, the format, the details and what dropping does: centred
    // (growing a touch as it's taken)
    let s = (area.height() / 560.0).clamp(0.55, 1.4) * (1.0 + 0.06 * out);
    let cx = area.center().x + shake;
    let big = FontId::new((96.0 * s).round(), FontFamily::Name("semibold".into()));
    let icon_size = (64.0 * s).round();
    let gap = 14.0 * s;
    let big_h = big.size;
    let block = icon_size + gap + big_h + 14.0 * s + 18.0 + 8.0 + 20.0;
    let mut y = area.center().y - block / 2.0;
    drop_icon(&p, pos2(cx, y + icon_size / 2.0), icon_size, d.icon, colour);
    y += icon_size + gap;
    gradient_text(&p, pos2(cx, y + big_h / 2.0), &d.big, big, colour);
    y += big_h + 14.0 * s;
    if !d.detail.is_empty() {
        let dc = Color32::from_rgb(214, 214, 222).gamma_multiply(alpha);
        let g = elide(&p, &d.detail, FontId::proportional(14.0), dc, area.width() * 0.8);
        p.galley(pos2(cx - g.size().x / 2.0, y), g, dc);
    }
    y += 18.0 + 8.0;
    let col = if d.playable { Color32::WHITE } else { Color32::from_rgb(200, 200, 206) };
    p.text(pos2(cx, y), Align2::CENTER_TOP, d.action, FontId::new(16.0, FontFamily::Name("semibold".into())), col.gamma_multiply(alpha));
}

/// An archive with several kinds of file in it (305 pictures, 160 videos) was
/// opened: the drop box stays up and asks which to play, a button for each.
/// The number of the one clicked.
pub(crate) fn draw_archive_choice(ctx: &egui::Context, archive: &str, kinds: &[(MediaKind, Vec<String>)], accent: Color32) -> Option<usize> {
    let screen = ctx.screen_rect();
    let area = Rect::from_min_max(
        pos2(screen.left(), screen.top() + WIN_BTN_H + 1.0),
        pos2(screen.right(), screen.bottom() - SEEK_FROM_BOTTOM - 12.0),
    );
    // (where the drop box was, so it carries straight on from the drop)
    let fr = area.shrink(10.0);
    if fr.height() < 40.0 {
        return None;
    }
    let mut picked = None;
    egui::Area::new(egui::Id::new("archive_choice")).order(egui::Order::Foreground).fixed_pos(fr.min).show(ctx, |ui| {
        // (clicks around the buttons don't reach the video underneath)
        ui.allocate_rect(fr, Sense::click_and_drag());
        let p = ui.painter().clone();
        let r = 16.0;
        crate::glass::fill(&p, fr, r, Color32::from_black_alpha(170), 1.0);
        p.rect_stroke(fr.shrink(1.0), r - 1.0, Stroke::new(2.0_f32, accent));

        let cx = fr.center().x;
        let semibold = |size: f32| FontId::new(size, FontFamily::Name("semibold".into()));
        // The question, the buttons, and how to back out: centred
        let name = Path::new(archive).file_name().map_or_else(|| archive.to_owned(), |n| n.to_string_lossy().into_owned());
        let q1 = p.layout_no_wrap("What do you want to play from".to_owned(), FontId::proportional(17.0), Color32::from_rgb(226, 226, 232));
        let q2 = elide(&p, &format!("{name}?"), semibold(17.0), Color32::WHITE, fr.width() - 60.0);
        let (btn_h, gap) = (58.0, 16.0);
        let block = q1.size().y + 4.0 + q2.size().y + 28.0 + btn_h + 22.0 + 16.0;
        let mut y = fr.center().y - block / 2.0;
        p.galley(pos2(cx - q1.size().x / 2.0, y), q1.clone(), Color32::WHITE);
        y += q1.size().y + 4.0;
        p.galley(pos2(cx - q2.size().x / 2.0, y), q2.clone(), Color32::WHITE);
        y += q2.size().y + 28.0;

        // A button for each kind: its icon, how many, and what they are
        let icon_size = 28.0;
        let parts: Vec<(DropIcon, std::sync::Arc<egui::Galley>, std::sync::Arc<egui::Galley>)> = kinds
            .iter()
            .map(|(k, files)| {
                let one = files.len() == 1;
                let (icon, noun) = match k {
                    MediaKind::Video => (DropIcon::Video, if one { "video" } else { "videos" }),
                    MediaKind::Music => (DropIcon::Music, if one { "song" } else { "songs" }),
                    MediaKind::Image => (DropIcon::Picture, if one { "picture" } else { "pictures" }),
                };
                let n = p.layout_no_wrap(files.len().to_string(), semibold(26.0), Color32::WHITE);
                let w = p.layout_no_wrap(noun.to_owned(), FontId::proportional(16.0), Color32::WHITE);
                (icon, n, w)
            })
            .collect();
        let widths: Vec<f32> = parts.iter().map(|(_, n, w)| (22.0 + icon_size + 12.0 + n.size().x + 8.0 + w.size().x + 24.0).round()).collect();
        let total = widths.iter().sum::<f32>() + gap * (widths.len().saturating_sub(1)) as f32;
        let mut x = (cx - total / 2.0).round();
        for (i, ((icon, n, w), bw)) in parts.into_iter().zip(&widths).enumerate() {
            let b = Rect::from_min_size(pos2(x, y), vec2(*bw, btn_h));
            let resp = ui.interact(b, ui.id().with(("archive_kind", i)), Sense::click());
            let hot = resp.hovered();
            if hot {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let (fill, edge) = if hot { (accent.gamma_multiply(0.16), accent) } else { (Color32::from_white_alpha(14), crate::glass::BORDER) };
            p.rect_filled(b, 12.0, fill);
            p.rect_stroke(b.shrink(0.5), 11.5, Stroke::new(1.0_f32, edge));
            let icon_col = if hot { accent } else { Color32::from_rgb(232, 232, 238) };
            let mut bx = b.left() + 22.0;
            drop_icon(&p, pos2(bx + icon_size / 2.0, b.center().y), icon_size, icon, &move |_| icon_col);
            bx += icon_size + 12.0;
            p.galley(pos2(bx, b.center().y - n.size().y / 2.0), n.clone(), Color32::WHITE);
            bx += n.size().x + 8.0;
            let dim = Color32::from_rgb(207, 207, 214);
            p.galley(pos2(bx, b.center().y - w.size().y / 2.0 + 2.0), w, dim);
            if resp.clicked() {
                picked = Some(i);
            }
            x += bw + gap;
        }
        y += btn_h + 22.0;
        p.text(pos2(cx, y), Align2::CENTER_TOP, "Esc to cancel", FontId::proportional(12.5), Color32::from_rgb(154, 154, 163));
    });
    picked
}

// ── Picture mode ──────────────────────────────────────────────────────────────
/// Zoom in / out: a magnifying glass with a plus or a minus
fn icon_zoom(p: &egui::Painter, c: Pos2, col: Color32, plus: bool) {
    let g = pos2(c.x - 1.5 * K, c.y - 1.5 * K);
    p.circle_stroke(g, 6.0 * K, Stroke::new(SW, col));
    kpoly(p, c, &[(2.9, 2.9), (7.5, 7.5)], col, SW);
    p.line_segment([pos2(g.x - 3.0 * K, g.y), pos2(g.x + 3.0 * K, g.y)], Stroke::new(SW, col));
    if plus {
        p.line_segment([pos2(g.x, g.y - 3.0 * K), pos2(g.x, g.y + 3.0 * K)], Stroke::new(SW, col));
    }
}

/// Set as wallpaper: a monitor showing a little landscape
fn icon_wallpaper(p: &egui::Painter, c: Pos2, col: Color32) {
    // (drawn on a 24-unit grid around the middle, like the other icons)
    let at = |x: f32, y: f32| pos2(c.x + (x - 12.0) * K, c.y + (y - 12.0) * K);
    p.rect_stroke(Rect::from_min_max(at(3.0, 4.0), at(21.0, 16.5)), 1.8, Stroke::new(SW, col));
    p.line_segment([at(12.0, 16.5), at(12.0, 20.5)], Stroke::new(SW, col));
    p.line_segment([at(8.5, 20.5), at(15.5, 20.5)], Stroke::new(SW, col));
    kpoly(p, c, &[(-6.5, 2.0), (-3.0, -1.5), (-0.2, 1.3), (1.8, -0.7), (5.0, 2.5)], col, SW);
    p.circle_stroke(at(15.6, 8.0), 1.2 * K, Stroke::new(SW, col));
}

/// Previous / next picture: a chevron, like the arrows at the window's edges
fn icon_chevron(p: &egui::Painter, c: Pos2, col: Color32, left: bool) {
    let s = if left { 1.0 } else { -1.0 };
    kpoly(p, c, &[(3.5 * s, -7.5), (-4.0 * s, 0.0), (3.5 * s, 7.5)], col, SW);
}

/// Fit to window: a small frame inside four corners
fn icon_fit(p: &egui::Painter, c: Pos2, col: Color32) {
    for (sx, sy) in [(-1.0_f32, -1.0_f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        kpoly(p, c, &[(8.0 * sx, 3.5 * sy), (8.0 * sx, 7.5 * sy), (4.0 * sx, 7.5 * sy)], col, SW);
    }
    p.rect_stroke(Rect::from_center_size(c, vec2(8.0 * K, 6.5 * K)), 1.0, Stroke::new(SW, col));
}

/// A picture's tools, in a frosted pill floating at the bottom in place of the
/// controls: previous / next, zoom out / the zoom / zoom in, fit to the window, its own size,
/// rotate, mirror, set as wallpaper (a file here), full screen
pub(crate) fn draw_picture_bar(app: &mut FluentMediaPlayer, ctx: &egui::Context, t: f32) {
    let screen = ctx.screen_rect();
    let white = Color32::WHITE.gamma_multiply(t);
    let accent = app.accent.gamma_multiply(t);
    let tint = |on: bool| if on { accent } else { white };
    let percent = app.picture_percent();
    let zoom_txt = percent.map_or_else(|| "\u{2013}".to_owned(), |z| format!("{z:.0}%"));

    // Laid out left to right: ‹ › | − 62% + | fit 1:1 | rotate mirror wallpaper | full screen
    const PAD: f32 = 8.0;
    const SEP: f32 = 17.0;
    const ZOOM_W: f32 = 50.0;
    let wallpaper = app.can_set_wallpaper();
    let w = PAD * 2.0 + BTN_STEP * 2.0 + SEP + BTN_STEP * 2.0 + ZOOM_W + SEP + BTN_STEP * 2.0 + SEP + BTN_STEP * if wallpaper { 3.0 } else { 2.0 } + SEP + BTN_STEP;
    let h = 42.0;
    let rect = Rect::from_min_size(pos2((screen.center().x - w / 2.0).round(), screen.bottom() - 12.0 - h), vec2(w, h));

    egui::Area::new(egui::Id::new("picture_bar")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        // (its blank parts are like the picture behind: right-click for the menu)
        let bg = ui.interact(rect, ui.id().with("pill_bg"), Sense::click_and_drag());
        if bg.hovered() {
            app.blank_hover = true;
        }
        let p = ui.painter().clone();
        // Lighter glass than the menus (it sits over the picture all the time);
        // edge and dividers as the menus have them
        crate::glass::fill_tinted(&p, rect, 14.0, crate::glass::MENU_BG.gamma_multiply(0.85), t, 0.42);
        p.rect_stroke(rect.shrink(0.5), 13.5, Stroke::new(1.0_f32, crate::glass::edge(crate::glass::BORDER).gamma_multiply(t)));

        let y = rect.center().y;
        let mut x = rect.left() + PAD;
        let sep = |p: &egui::Painter, x: f32| {
            p.line_segment([pos2(x, y - 9.0), pos2(x, y + 9.0)], Stroke::new(1.0_f32, crate::glass::BORDER.gamma_multiply(t)));
        };
        let button = |ui: &mut egui::Ui, x: &mut f32, id: &str, tip: &str| {
            let c = pos2(*x + BTN_STEP / 2.0, y);
            *x += BTN_STEP;
            (hit(ui, c, id, tip, t).activated(), c)
        };

        // Previous / next picture (greyed out at either end of the folder)
        for (delta, left) in [(-1_isize, true), (1, false)] {
            let tip = if left { "Previous picture (\u{2190})" } else { "Next picture (\u{2192})" };
            let c = pos2(x + BTN_STEP / 2.0, y);
            x += BTN_STEP;
            let resp = hit(ui, c, if left { "pic_prev" } else { "pic_next" }, tip, t);
            let can = app.step_index(delta).is_some();
            icon_chevron(&p, c, if can { white } else { white.gamma_multiply(0.35) }, left);
            if can {
                step_clicks(app, ctx, &resp, delta);
            }
        }
        sep(&p, x + SEP / 2.0);
        x += SEP;

        let (go, c) = button(ui, &mut x, "zoom_out", "Zoom out (mouse wheel)");
        icon_zoom(&p, c, white, false);
        if go {
            app.picture_zoom_by(-1.0, None);
        }
        // The zoom (just shown: fit has its own button)
        let zr = Rect::from_min_size(pos2(x, y - 15.0), vec2(ZOOM_W, 30.0));
        p.text(zr.center(), Align2::CENTER_CENTER, &zoom_txt, FontId::new(12.5, FontFamily::Name("semibold".into())), white);
        x += ZOOM_W;
        let (go, c) = button(ui, &mut x, "zoom_in", "Zoom in (mouse wheel)");
        icon_zoom(&p, c, white, true);
        if go {
            app.picture_zoom_by(1.0, None);
        }

        sep(&p, x + SEP / 2.0);
        x += SEP;
        let (go, c) = button(ui, &mut x, "fit", "Fit to window");
        icon_fit(&p, c, tint(app.pic_zoom == 0.0 && app.pic_pan == (0.0, 0.0)));
        if go {
            app.picture_fit();
        }
        let (go, c) = button(ui, &mut x, "actual", "Actual size (100%)");
        p.text(c, Align2::CENTER_CENTER, "1:1", FontId::new(12.5, FontFamily::Name("semibold".into())), tint(app.picture_is_actual()));
        if go {
            app.picture_actual();
        }

        sep(&p, x + SEP / 2.0);
        x += SEP;
        let (go, c) = button(ui, &mut x, "rotate", "Rotate 90\u{b0} (R)");
        icon_rotate(&p, c, tint(app.rotation != 0));
        if go {
            app.rotate_90();
        }
        let (go, c) = button(ui, &mut x, "mirror", "Mirror");
        icon_mirror(&p, c, tint(app.mirror));
        if go {
            app.toggle_mirror();
        }
        if wallpaper {
            let (go, c) = button(ui, &mut x, "wallpaper", "Set as wallpaper");
            icon_wallpaper(&p, c, white);
            if go {
                app.set_wallpaper();
            }
        }

        sep(&p, x + SEP / 2.0);
        x += SEP;
        let (go, c) = button(ui, &mut x, "fs", "Full screen (Enter)");
        icon_fullscreen(&p, c, white);
        if go {
            app.toggle_fullscreen(ctx);
        }
    });
}

/// Where a picture's previous (left) / next arrow is: 70 × 160 at the side,
/// in the middle
pub(crate) fn picture_arrow_rect(screen: Rect, left: bool) -> Rect {
    let (w, h) = (70.0, 160.0_f32.min(screen.height() - TOP_H - 90.0).max(60.0));
    let x0 = if left { screen.left() } else { screen.right() - w };
    Rect::from_min_size(pos2(x0, (screen.center().y - h / 2.0).round()), vec2(w, h))
}

/// A previous / next picture button's clicks. Pressing it starts opening that
/// picture already, unseen; letting go on it shows it, letting go elsewhere
/// forgets it (see step_press)
fn step_clicks(app: &mut FluentMediaPlayer, ctx: &egui::Context, resp: &egui::Response, delta: isize) {
    let (pressed, released, from, at) =
        ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.press_origin(), i.pointer.latest_pos()));
    if pressed && from.is_some_and(|p| resp.rect.contains(p)) {
        app.step_press(delta, resp.id);
    }
    if released && app.step_pressed(resp.id) {
        app.step_release(at.is_some_and(|p| resp.rect.contains(p)));
    } else if resp.clicked() {
        app.play_adjacent(delta);
    }
}

/// Previous / next picture: a tall strip with a thin arrow at each side of the
/// window, shown while the pointer is on it
pub(crate) fn draw_picture_arrows(app: &mut FluentMediaPlayer, ctx: &egui::Context) {
    if app.folder_files.len() < 2 {
        return;
    }
    let screen = ctx.screen_rect();
    for (delta, left) in [(-1_isize, true), (1, false)] {
        // (not on the side the playlist is open on)
        if app.step_index(delta).is_none() || (!left && app.adjust_ui.playlist_open) {
            continue;
        }
        let id = egui::Id::new(("picture_arrow", left));
        let mut r = picture_arrow_rect(screen, left);
        // (clear of the playlist's tab on the right edge, should a small window
        // leave no room for the tab above it: below it)
        let tab = app.adjust_ui.playlist_tab;
        if !left && tab.is_positive() && r.intersects(tab.expand(6.0)) {
            r = r.translate(vec2(0.0, tab.bottom() + 8.0 - r.top()));
        }
        egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(r.min).show(ctx, |ui| {
            // (a drag that starts on it is its own: not passed on to the picture
            // underneath, which would move it or the window)
            let resp = ui.interact(r, id.with("hit"), Sense::click_and_drag());
            let hot = resp.hovered();
            let a = ctx.animate_bool_with_time(id, hot, 0.12);
            if hot {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if a > 0.0 {
                let p = ui.painter();
                let rounding = if left {
                    egui::Rounding { nw: 0.0, sw: 0.0, ne: 10.0, se: 10.0 }
                } else {
                    egui::Rounding { nw: 10.0, sw: 10.0, ne: 0.0, se: 0.0 }
                };
                p.rect_filled(r, rounding, Color32::from_black_alpha((128.0 * a) as u8));
                // A thin chevron, 44 px tall (a little toward the window's edge, so it
                // looks centred: a chevron's weight is in its open side)
                let c = r.center() + vec2(if left { -3.0 } else { 3.0 }, 0.0);
                let dx = if left { 10.0 } else { -10.0 };
                // (one joined line, 2.6 px, so the point is clean; bolder than before)
                let stroke = Stroke::new(2.6_f32, Color32::WHITE.gamma_multiply(a));
                p.add(egui::Shape::line(vec![pos2(c.x + dx, c.y - 22.0), pos2(c.x - dx, c.y), pos2(c.x + dx, c.y + 22.0)], stroke));
            }
            step_clicks(app, ctx, &resp, delta);
        });
    }
}
