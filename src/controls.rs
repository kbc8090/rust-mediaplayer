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
    let r = 5.0;

    glass::shadow(&p, rect, r, alpha * 0.6);
    glass::fill(&p, rect, r, Color32::from_rgba_unmultiplied(43, 43, 43, 245), alpha);
    let edge = glass::edge(Color32::from_white_alpha(20));
    p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, edge.gamma_multiply(alpha)));
    p.galley(rect.min + pad, galley, Color32::WHITE);
}

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

pub(crate) fn draw_knob(p: &egui::Painter, ppp: f32, centre: Pos2, hot: bool, accent: Color32, t: f32) {
    let snap = |v: f32| (v * ppp).round() / ppp;
    let c = pos2(snap(centre.x), snap(centre.y));
    let (ring, dot) = if hot { (7.5, 4.6) } else { (6.5, 3.9) };
    adjust::disc(p, pos2(c.x, c.y + 1.0), ring + 0.8, Color32::from_black_alpha((80.0 * t) as u8));
    adjust::disc(p, c, ring, KNOB_RING.gamma_multiply(t));
    adjust::disc(p, c, dot, accent);
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
                if bg.double_clicked() {
                    app.toggle_fullscreen(ctx);
                }
            }

            // Drag region: move the window; double-click goes fullscreen
            // (The whole strip, not just its top part: the strip covers the video,
            // so any part of it that doesn't drag is a dead spot.)
            if !fullscreen {
                let drag = ui.interact(bar, ui.id().with("drag"), Sense::click_and_drag());
                let pressed_on_edge = ctx
                    .input(|i| i.pointer.press_origin())
                    .is_some_and(|o| hit_edge(o, screen).is_some());
                if drag.drag_started() && !pressed_on_edge {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                // (fullscreen, like double-clicking the video; the maximise button
                // and dragging to the top of the screen still maximise)
                if drag.double_clicked() {
                    app.toggle_fullscreen(ctx);
                }
            }

            // Left: menu button, then the file name level with it
            let mc = pos2(MENU_X, MENU_Y);
            let menu = hit_at(ui, mc, "menu", "Menu", t, false);
            app.menu_anchor = menu.rect;
            if app.main_menu && !menu.hovered() {
                // keep it highlighted while its menu is open
                p.rect_filled(Rect::from_center_size(mc, vec2(BTN_BOX, BTN_BOX)), 8.0, Color32::from_white_alpha((8.0 * t) as u8));
            }
            for dy in [-4.5, 0.0, 4.5] {
                p.line_segment([pos2(mc.x - 7.0, mc.y + dy), pos2(mc.x + 7.0, mc.y + dy)], Stroke::new(SW, white));
            }
            if menu.activated() {
                app.main_menu = !app.main_menu;
            }

            // (text centred on its line box sits a little low next to the icon)
            let y = MENU_Y - 1.0;
            let right_limit = if fullscreen { screen.right() - 20.0 } else { screen.right() - WIN_BTN_W * 3.0 - 10.0 };
            let tx = TITLE_X;
            let clip = p.with_clip_rect(Rect::from_min_max(pos2(tx, 0.0), pos2(right_limit, TOP_H)));
            let font = FontId::new(14.5, FontFamily::Name("semibold".into()));
            clip.text(pos2(tx + 1.0, y + 1.0), Align2::LEFT_CENTER, &title, font.clone(), black(170, t));
            clip.text(pos2(tx, y), Align2::LEFT_CENTER, &title, font, white);

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
                        if k == 2 {
                            hit_r.max.x -= BTN_EDGE;
                        }
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
            if bg.double_clicked() {
                app.toggle_fullscreen(ctx);
            }
            let pressed_on_edge = ctx
                .input(|i| i.pointer.press_origin())
                .is_some_and(|o| hit_edge(o, screen).is_some());
            if bg.drag_started() && !app.is_fullscreen && !pressed_on_edge {
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
            let dragging = seek.is_pointer_button_down_on() || seek.dragged();
            if dragging {
                if let Some(pos) = seek.interact_pointer_pos() {
                    let f = frac_at(pos.x);
                    app.drag_frac = Some(f);
                    if app.duration > 0.0 { app.seek_to(f as f64 * app.duration); }
                }
            } else {
                app.drag_frac = None;
            }
            let frac = app.drag_frac.unwrap_or(if app.duration > 0.0 {
                (app.time_pos / app.duration).clamp(0.0, 1.0) as f32
            } else { 0.0 });
            let hot = seek.hovered() || dragging;

            let track_h = if hot { 4.0 } else { 3.0 };
            let (y0, y1) = (seek_y - track_h / 2.0, seek_y + track_h / 2.0);
            pill(&p, Rect::from_min_max(pos2(sr.left(), y0), pos2(sr.right(), y1)), TRACK.gamma_multiply(t));
            let fill_x = k0 + kw * frac;
            pill(&p, Rect::from_min_max(pos2(sr.left(), y0), pos2(fill_x, y1)), accent);
            // Chapter marks: small gaps in the bar
            if app.duration > 0.0 {
                let ppp = ctx.pixels_per_point();
                for &(start, _) in app.chapters.iter() {
                    if start > 0.5 && start < app.duration - 0.5 {
                        let x = ((k0 + kw * (start / app.duration) as f32) * ppp).round() / ppp;
                        p.rect_filled(
                            Rect::from_center_size(pos2(x, seek_y), vec2(2.0, track_h)),
                            0.0, Color32::from_black_alpha((200.0 * t) as u8),
                        );
                    }
                }
            }
            draw_knob(&p, ctx.pixels_per_point(), pos2(fill_x, seek_y), hot, accent, t);

            // Hover / drag time preview
            let preview = if dragging { app.drag_frac }
                else if seek.hovered() { seek.hover_pos().map(|pp| frac_at(pp.x)) }
                else { None };
            if let (Some(f), true) = (preview, app.duration > 0.0) {
                let secs = f as f64 * app.duration;
                let txt = fmt_time(secs);
                let font = FontId::proportional(13.0);
                let w = p.layout_no_wrap(txt.clone(), font.clone(), Color32::WHITE).size().x + 16.0;
                let cx = (k0 + kw * f).clamp(sr.left() + w / 2.0, sr.right() - w / 2.0);
                let r = Rect::from_center_size(pos2(cx, seek_y - 28.0), vec2(w, 23.0));
                p.rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(32, 32, 32, (235.0 * t) as u8));
                p.text(r.center(), Align2::CENTER_CENTER, txt, font, white);

                // Thumbnail of that moment, above the time (not for links: that
                // would mean fetching the video a second time)
                let thumbs_ok = !is_url(&app.current_file);
                if thumbs_ok {
                    app.thumbs.request(ctx, secs);
                }
                if let Some(aspect) = app.thumbs.aspect().filter(|_| thumbs_ok) {
                    let tw = (thumbs::THUMB_H * aspect).clamp(64.0, 200.0);
                    let th = (tw / aspect).clamp(40.0, 150.0);
                    let ppp = ctx.pixels_per_point();
                    let snap = |v: f32| (v * ppp).round() / ppp;
                    let tx = (k0 + kw * f - tw / 2.0).clamp(screen.left() + 8.0, screen.right() - 8.0 - tw);
                    let tr = Rect::from_min_size(pos2(snap(tx), snap(r.top() - 6.0 - th)), vec2(snap(tw), snap(th)));
                    let tp = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("thumb")));
                    for (e, a) in [(1.0, 60.0), (2.5, 30.0), (4.0, 14.0)] {
                        tp.rect_filled(tr.expand(e), 4.0 + e, Color32::from_black_alpha((a * t) as u8));
                    }
                    app.thumbs.paint(&tp, tr, 2.0);
                    tp.rect_stroke(tr.expand(0.5), 2.5, Stroke::new(1.0_f32, { let a = (70.0 * t) as u8; Color32::from_rgba_premultiplied(a, a, a, a) }));
                }
            }

            // ── Button row ────────────────────────────────────────────────────
            let row_y = screen.bottom() - ROW_FROM_BOTTOM;
            let left = screen.left();
            let right = screen.right();

            let time_text = format!("{} / {}", fmt_time(app.time_pos), fmt_time(app.duration));
            let time_font = FontId::new(14.5, FontFamily::Name("semibold".into()));
            let time_w = p.layout_no_wrap(time_text.clone(), time_font.clone(), white).size().x;
            let lay = row_layout(left, right, time_w);

            // Play / pause
            let c = pos2(lay.play, row_y);
            if hit(ui, c, "play", if app.is_playing { "Pause (Space)" } else { "Play (Space)" }, t).activated() {
                app.toggle_play_pause();
            }
            if app.is_playing { icon_pause(&p, c, white) } else { icon_play(&p, c, white) }

            // Previous / next file in the same folder (dimmed when there isn't one)
            let dim = white.gamma_multiply(0.4);
            let c = pos2(lay.prev, row_y);
            if app.has_prev() && hit(ui, c, "prev", "Previous file (P)", t).activated() { app.play_adjacent(-1); }
            icon_prev(&p, c, if app.has_prev() { white } else { dim });

            let c = pos2(lay.next, row_y);
            if app.has_next() && hit(ui, c, "next", "Next file (N)", t).activated() { app.play_adjacent(1); }
            icon_next(&p, c, if app.has_next() { white } else { dim });

            // Time
            p.text(pos2(lay.time_x, row_y - 1.0), Align2::LEFT_CENTER, time_text, time_font, white);

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
            draw_knob(&p, ctx.pixels_per_point(), pos2(vl + vw * vf, row_y), vhot, accent, t);
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

            let c = pos2(lay.repeat, row_y);
            if hit(ui, c, "repeat", "Repeat this video", t).activated() { app.toggle_repeat(); }
            icon_repeat(&p, c, tint(app.repeat));

            let c = pos2(lay.captions, row_y);
            if hit(ui, c, "captions", "Cycle subtitles", t).activated() { app.cycle_captions(); }
            icon_captions(&p, c, white);

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
    pub(crate) captions: f32,
    pub(crate) ontop: f32,
    pub(crate) speed: f32,
    /// The last button, flush with the seek bar's right end
    pub(crate) fullscreen: f32,
}

pub(crate) fn row_layout(left: f32, right: f32, time_w: f32) -> RowLayout {
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
    let repeat = captions - BTN_STEP;

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
    RowLayout { play, prev, next, time_x, mute, vol_x, rotate, mirror, repeat, captions, ontop, speed, fullscreen }
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
