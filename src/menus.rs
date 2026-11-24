//! Menus and panels: the main menu (with its submenu), the playback speed menu, the keyboard
//! shortcuts panel and the info panel (Tab).

use super::*;

// ── Keyboard shortcuts panel (menu) ───────────────────────────────────────────
/// Keys (each drawn as a key cap) and what they do
pub(crate) const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["Space"], "Play / pause"),
    (&["\u{2190}", "\u{2192}"], "Back / forward 5 seconds"),
    (&["D"], "Previous frame"),
    (&["F"], "Next frame"),
    (&["\u{2191}", "\u{2193}"], "Volume up / down"),
    (&["Mouse wheel"], "Volume up / down"),
    (&["M"], "Mute"),
    (&["P"], "Previous file"),
    (&["N"], "Next file"),
    (&["Enter"], "Full screen"),
    (&["Double-click"], "Full screen"),
    (&["Right-click"], "Main menu"),
    (&["Esc"], "Leave full screen / close menu"),
    (&["R"], "Rotate 90\u{b0}"),
    (&["Ctrl+O"], "Open file(s)"),
    (&["Ctrl+U"], "Open URL"),
    (&["Ctrl+S"], "Save video as (online videos)"),
    (&["Ctrl+Shift+S"], "Save clip as"),
    (&["[  ]"], "Clip: start / end at the playhead"),
    (&["Z"], "Clip: zoom the seek bar in / out"),
    (&["Ctrl+V"], "Play a copied link"),
    (&["E"], "Picture & Sound"),
    (&["L"], "Playlist (while one plays)"),
    (&["Tab"], "Video info"),
    (&["H"], "Next decoding method"),
    (&["Media keys"], "Play / pause, next, previous"),
];

pub(crate) fn draw_shortcuts(app: &mut FluentMediaPlayer, ctx: &egui::Context) {
    if !app.show_keys {
        return;
    }
    const W: f32 = 380.0;
    const HEAD_H: f32 = 48.0;
    const ROW_H: f32 = 28.0;
    const PAD: f32 = 10.0;
    let screen = ctx.screen_rect();
    let full_h = HEAD_H + SHORTCUTS.len() as f32 * ROW_H + PAD;
    let size = vec2(W.min(screen.width() - 24.0), full_h.min(screen.height() - 40.0));
    let ppp = ctx.pixels_per_point();
    let r0 = Rect::from_center_size(screen.center(), size);
    let rect = Rect::from_min_size(pos2((r0.left() * ppp).round() / ppp, (r0.top() * ppp).round() / ppp), size);

    // A click anywhere outside the panel closes it
    let pressed_outside = ctx.input(|i| {
        i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|o| !rect.contains(o))
    });
    if pressed_outside {
        app.show_keys = false;
        return;
    }

    let id = egui::Id::new("shortcuts");
    let mut close = false;
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            // Keep the player's clicks / drags away from the video behind
            ui.allocate_rect(screen, Sense::click_and_drag());
            let p = ui.painter().clone();
            let r = 8.0;
            glass::shadow(&p, rect, r, 1.0);
            glass::fill(&p, rect, r, glass::MENU_BG, 1.0);
            p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, glass::edge(glass::BORDER)));
            ui.interact(rect, ui.id().with("panel"), Sense::click_and_drag());

            let text = Color32::from_rgb(240, 240, 240);
            p.text(
                pos2(rect.left() + 18.0, rect.top() + HEAD_H / 2.0),
                Align2::LEFT_CENTER,
                "Keyboard shortcuts",
                FontId::new(15.0, FontFamily::Name("semibold".into())),
                text,
            );

            // Close button (top right)
            let xr = Rect::from_center_size(pos2(rect.right() - 24.0, rect.top() + HEAD_H / 2.0), vec2(30.0, 30.0));
            let xb = ui.interact(xr, ui.id().with("close"), Sense::click_and_drag());
            if let Some(c) = adjust::state_fill(&xb) {
                p.rect_filled(xr, 4.0, c);
            }
            let c = xr.center();
            for (a, b) in [((-5.0, -5.0), (5.0, 5.0)), ((-5.0, 5.0), (5.0, -5.0))] {
                p.line_segment([c + vec2(a.0, a.1), c + vec2(b.0, b.1)], Stroke::new(1.2_f32, text));
            }
            if xb.activated() {
                close = true;
            }
            p.line_segment(
                [pos2(rect.left() + 1.0, rect.top() + HEAD_H - 0.5), pos2(rect.right() - 1.0, rect.top() + HEAD_H - 0.5)],
                Stroke::new(1.0_f32, glass::BORDER),
            );

            // The list (scrolls if the window is too short for all of it)
            let list = Rect::from_min_max(pos2(rect.left(), rect.top() + HEAD_H + 4.0), pos2(rect.right(), rect.bottom() - 6.0));
            let mut child = ui.child_ui(list, egui::Layout::top_down(egui::Align::Min));
            egui::ScrollArea::vertical().max_height(list.height()).show(&mut child, |ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for (keys, what) in SHORTCUTS {
                    let (row, _) = ui.allocate_exact_size(vec2(list.width() - 8.0, ROW_H), Sense::hover());
                    let p = ui.painter();
                    let mut x = row.left() + 18.0;
                    for key in keys.iter() {
                        let g = p.layout_no_wrap(
                            key.to_string(),
                            FontId::new(12.5, FontFamily::Name("semibold".into())),
                            text,
                        );
                        let cap = Rect::from_min_size(pos2(x, row.center().y - 10.5), vec2((g.size().x + 14.0).max(24.0), 21.0));
                        p.rect_filled(cap, 4.0, Color32::from_rgb(64, 64, 66));
                        p.rect_filled(
                            Rect::from_min_max(pos2(cap.left() + 1.0, cap.bottom() - 2.0), pos2(cap.right() - 1.0, cap.bottom())),
                            2.0,
                            Color32::from_rgb(52, 52, 54),
                        );
                        p.galley(cap.center() - g.size() / 2.0 - vec2(0.0, 0.5), g, text);
                        x = cap.right() + 6.0;
                    }
                    p.text(pos2(row.left() + 150.0, row.center().y), Align2::LEFT_CENTER, *what, FontId::proportional(13.5), text);
                }
            });
        });
    ctx.move_to_top(egui::LayerId::new(egui::Order::Foreground, id));
    if close {
        app.show_keys = false;
    }
}

// ── Main menu (top left) ──────────────────────────────────────────────────────
pub(crate) enum MenuRow {
    Item { key: &'static str, label: &'static str, hint: Option<&'static str>, mark: Mark },
    /// Opens a submenu to the right; `hint` is the current choice, shown at the right
    Sub { key: &'static str, label: &'static str, hint: &'static str },
    /// Centred heading
    Title(&'static str),
    /// An item that can't be used right now (greyed out, does nothing)
    Off { label: &'static str, hint: Option<&'static str> },
    Sep,
}

/// What's drawn in front of a menu item: nothing, a check mark (on/off setting) or a
/// dot (one choice out of several, like Explorer's "Sort by")
pub(crate) enum Mark {
    None,
    Check(bool),
    Dot(bool),
}

pub(crate) const SORT_CHOICES: [(SortBy, &str); 4] = [
    (SortBy::Name, "Name"),
    (SortBy::Modified, "Date modified"),
    (SortBy::Created, "Date created"),
    (SortBy::Size, "Size"),
];

/// "Ascending" / "Descending" in words that fit the sort
pub(crate) fn order_labels(by: SortBy) -> (&'static str, &'static str) {
    match by {
        SortBy::Name => ("A to Z", "Z to A"),
        SortBy::Size => ("Smallest first", "Largest first"),
        SortBy::Modified | SortBy::Created => ("Oldest first", "Newest first"),
    }
}

/// The layer popup menus (main, speed, text field, dropdowns) live on: above every
/// panel. (On the panels' own layer they'd end up underneath one once they'd been
/// closed and opened again, because egui keeps the last stacking order.)
pub(crate) const POPUP: egui::Order = egui::Order::Debug;

pub(crate) const MENU_ITEM_H: f32 = 28.0;

/// From a menu's edge to its rows' highlights (the same as in the panels)
pub(crate) const MENU_PAD: f32 = adjust::EDGE;

/// A separator row: its line with a 4 px gap above and below
pub(crate) const MENU_SEP_H: f32 = 9.0;

/// Hovering a row this long opens its submenu (or closes another one)
pub(crate) const SUBMENU_DELAY: f32 = 0.3;

pub(crate) fn menu_row_h(r: &MenuRow) -> f32 {
    MenuSize::NORMAL.row_h(r)
}

/// Row sizes and fonts of a menu: the main menu, or the smaller speed menu
#[derive(Clone, Copy)]
pub(crate) struct MenuSize {
    pub(crate) item_h: f32,
    pub(crate) sep_h: f32,
    pub(crate) font: f32,
    pub(crate) small_font: f32,
    /// Distance of the text / the mark from the row's left edge
    pub(crate) text_x: f32,
    pub(crate) mark_x: f32,
}

impl MenuSize {
    const NORMAL: Self = Self { item_h: MENU_ITEM_H, sep_h: MENU_SEP_H, font: 14.5, small_font: 13.5, text_x: 28.0, mark_x: 14.0 };
    const COMPACT: Self = Self { item_h: 22.0, sep_h: MENU_SEP_H, font: 13.0, small_font: 12.5, text_x: 22.0, mark_x: 11.0 };

    pub(crate) fn row_h(&self, r: &MenuRow) -> f32 {
        match r {
            MenuRow::Sep => self.sep_h,
            _ => self.item_h,
        }
    }
}

/// Glass panel with rows; returns (row picked, hovered row key, rect of each Sub row)
pub(crate) fn draw_menu_rows(
    ui: &mut egui::Ui,
    rect: Rect,
    rows: &[MenuRow],
    sub_open: Option<&'static str>,
    selected: Option<Color32>,
    size: MenuSize,
) -> (Option<&'static str>, Option<&'static str>, Vec<(&'static str, Rect)>) {
    // The menu's own background swallows clicks (so they don't reach the video)
    ui.allocate_rect(rect, Sense::click_and_drag());
    let p = ui.painter().clone();
    let r = 8.0;
    glass::shadow(&p, rect, r, 1.0);
    glass::fill(&p, rect, r, glass::MENU_BG, 1.0);
    p.rect_stroke(rect.shrink(0.5), r, Stroke::new(1.0_f32, glass::edge(glass::BORDER)));

    let text = Color32::from_rgb(240, 240, 240);
    let dim = Color32::from_rgb(160, 160, 160);
    let font = FontId::proportional(size.font);
    let small = FontId::proportional(size.small_font);
    let text_x = rect.left() + MENU_PAD + size.text_x;
    let mark_x = rect.left() + MENU_PAD + size.mark_x;
    let (mut picked, mut hovered, mut subs) = (None, None, Vec::new());

    let mut top = rect.top() + MENU_PAD;
    for row in rows {
        let h = size.row_h(row);
        let cy = top + h / 2.0;
        let ir = Rect::from_min_size(pos2(rect.left() + MENU_PAD, top), vec2(rect.width() - 2.0 * MENU_PAD, h));
        match row {
            MenuRow::Sep => {
                p.line_segment(
                    [pos2(rect.left() + 1.0, cy), pos2(rect.right() - 1.0, cy)],
                    Stroke::new(1.0_f32, glass::BORDER),
                );
            }
            MenuRow::Title(label) => {
                p.text(pos2(rect.center().x, cy), Align2::CENTER_CENTER, *label, small.clone(), dim);
            }
            MenuRow::Off { label, hint } => {
                let faint = Color32::from_rgb(110, 110, 110);
                p.text(pos2(text_x, cy), Align2::LEFT_CENTER, *label, font.clone(), faint);
                if let Some(hint) = hint {
                    p.text(pos2(rect.right() - MENU_PAD - 10.0, cy), Align2::RIGHT_CENTER, *hint, small.clone(), faint);
                }
            }
            MenuRow::Item { key, label, hint, mark } => {
                let resp = ui.interact(ir, ui.id().with(*key), Sense::click_and_drag());
                // The current choice (speed menu): tinted with the accent colour
                if let (Some(sel), Mark::Dot(true)) = (selected, mark) {
                    p.rect_filled(ir, 4.0, sel);
                }
                if let Some(c) = adjust::state_fill(&resp) {
                    p.rect_filled(ir, 4.0, c);
                }
                match mark {
                    Mark::Check(true) => {
                        poly(&p, pos2(mark_x, cy), &[(-4.5, 0.0), (-1.5, 3.2), (4.5, -3.5)], text, 1.5);
                    }
                    Mark::Dot(true) => {
                        adjust::disc(&p, pos2(mark_x, cy), if size.item_h < MENU_ITEM_H { 2.5 } else { 3.0 }, text);
                    }
                    _ => {}
                }
                p.text(pos2(text_x, cy), Align2::LEFT_CENTER, *label, font.clone(), text);
                if let Some(hint) = hint {
                    p.text(pos2(rect.right() - MENU_PAD - 10.0, cy), Align2::RIGHT_CENTER, *hint, small.clone(), dim);
                }
                if resp.hovered() {
                    hovered = Some(*key);
                }
                if resp.activated() {
                    picked = Some(*key);
                }
            }
            MenuRow::Sub { key, label, hint } => {
                let resp = ui.interact(ir, ui.id().with(*key), Sense::click_and_drag());
                // Highlighted while hovered, or while its submenu is open
                let fill = adjust::state_fill(&resp).or((sub_open == Some(*key)).then_some(adjust::HOVER));
                if let Some(c) = fill {
                    p.rect_filled(ir, 4.0, c);
                }
                p.text(pos2(text_x, cy), Align2::LEFT_CENTER, *label, font.clone(), text);
                let c = pos2(rect.right() - MENU_PAD - 13.0, cy);
                p.line_segment([c + vec2(-2.5, -4.5), c + vec2(2.0, 0.0)], Stroke::new(1.3_f32, text));
                p.line_segment([c + vec2(2.0, 0.0), c + vec2(-2.5, 4.5)], Stroke::new(1.3_f32, text));
                p.text(pos2(c.x - 14.0, cy), Align2::RIGHT_CENTER, *hint, small.clone(), dim);
                if resp.hovered() {
                    hovered = Some(*key);
                }
                if resp.activated() {
                    picked = Some(*key);
                }
                subs.push((*key, ir));
            }
        }
        top += h;
    }
    (picked, hovered, subs)
}

/// A text field's right-click menu at `at`: Cut, Copy, Paste, Delete, Select all
/// (greyed out when there's nothing to cut / paste / select). Returns the item
/// picked, or "close" for a press outside it.
pub(crate) fn draw_text_menu(ctx: &egui::Context, at: Pos2, selected: bool, can_paste: bool, has_text: bool) -> Option<&'static str> {
    let item = |on: bool, key: &'static str, label: &'static str, hint: &'static str| {
        if on {
            MenuRow::Item { key, label, hint: Some(hint), mark: Mark::None }
        } else {
            MenuRow::Off { label, hint: Some(hint) }
        }
    };
    let rows = [
        item(selected, "cut", "Cut", "Ctrl+X"),
        item(selected, "copy", "Copy", "Ctrl+C"),
        item(can_paste, "paste", "Paste", "Ctrl+V"),
        item(selected, "delete", "Delete", "Del"),
        MenuRow::Sep,
        item(has_text, "select_all", "Select all", "Ctrl+A"),
    ];
    let screen = ctx.screen_rect();
    let size = vec2(200.0, MENU_PAD * 2.0 + rows.iter().map(menu_row_h).sum::<f32>());
    let x = if at.x + size.x <= screen.right() - 4.0 { at.x } else { at.x - size.x };
    let y = if at.y + size.y <= screen.bottom() - 4.0 { at.y } else { at.y - size.y };
    let rect = Rect::from_min_size(pos2(x.max(screen.left() + 4.0), y.max(screen.top() + 4.0)), size);
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|o| !rect.contains(o)));
    if outside {
        return Some("close");
    }
    let id = egui::Id::new("text_menu");
    let mut picked = None;
    egui::Area::new(id).order(POPUP).fixed_pos(rect.min).show(ctx, |ui| {
        picked = draw_menu_rows(ui, rect, &rows, None, None, MenuSize::NORMAL).0;
    });
    ctx.move_to_top(egui::LayerId::new(POPUP, id));
    picked
}

/// Is `p` inside the triangle a, b, c (either winding)?
pub(crate) fn in_triangle(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let side = |u: Pos2, v: Pos2| (v.x - u.x) * (p.y - u.y) - (v.y - u.y) * (p.x - u.x);
    let (d1, d2, d3) = (side(a, b), side(b, c), side(c, a));
    !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
}

pub(crate) fn draw_main_menu(app: &mut FluentMediaPlayer, ctx: &egui::Context) {
    if !app.main_menu {
        app.menu_sub = None;
        app.menu_hover = None;
        app.menu_aim = None;
        app.menu_at = None;
        return;
    }
    const W: f32 = 272.0;
    const SUB_W: f32 = 190.0;

    let sort_label = SORT_CHOICES.iter().find(|(by, _)| *by == app.sort_by).map_or("Name", |(_, l)| *l);
    let mut rows = vec![
        MenuRow::Item { key: "open", label: "Open file(s)\u{2026}", hint: Some("Ctrl+O"), mark: Mark::None },
        MenuRow::Item { key: "open_url", label: "Open URL\u{2026}", hint: Some("Ctrl+U"), mark: Mark::None },
    ];
    // Save As… while an online video is playing (Cancel saving while one saves)
    if app.has_file && is_url(&app.current_file) {
        rows.push(if app.save.as_ref().is_some_and(|j| j.running()) {
            MenuRow::Item { key: "cancel_save", label: "Cancel saving", hint: None, mark: Mark::None }
        } else {
            MenuRow::Item { key: "save", label: "Save video as\u{2026}", hint: Some("Ctrl+S"), mark: Mark::None }
        });
    }
    // Save clip as… for anything playing (a link or a file here)
    if app.has_file && app.duration > 1.0 && !app.live {
        rows.push(MenuRow::Item { key: "clip", label: "Save clip as\u{2026}", hint: Some("Ctrl+Shift+S"), mark: Mark::None });
    }
    rows.extend([
        MenuRow::Sep,
        MenuRow::Item { key: "adjust", label: "Picture & Sound", hint: Some("E"), mark: Mark::None },
        MenuRow::Item { key: "online", label: "Online video settings", hint: None, mark: Mark::None },
        MenuRow::Item { key: "snap", label: "Snap to screen edges", hint: None, mark: Mark::Check(app.snap) },
        MenuRow::Item { key: "multi", label: "Allow multiple windows", hint: None, mark: Mark::Check(app.multi) },
        MenuRow::Item { key: "border", label: "Accent color border", hint: None, mark: Mark::Check(app.border) },
        MenuRow::Item { key: "glass", label: "Frosted glass menus", hint: None, mark: Mark::Check(app.glass) },
        MenuRow::Item { key: "right_click", label: "Right-click for main menu", hint: None, mark: Mark::Check(app.right_click_menu) },
        MenuRow::Sep,
        MenuRow::Item { key: "autoplay", label: "Auto-play next file in folder", hint: None, mark: Mark::Check(app.autoplay) },
        MenuRow::Sub { key: "sort", label: "Sort folder by", hint: sort_label },
        MenuRow::Sep,
        MenuRow::Item { key: "keys", label: "Keyboard shortcuts", hint: None, mark: Mark::None },
        MenuRow::Item { key: "quit", label: "Quit", hint: Some("Alt+F4"), mark: Mark::None },
    ]);
    let (asc, desc) = order_labels(app.sort_by);
    let mut sub_rows: Vec<MenuRow> = SORT_CHOICES
        .iter()
        .map(|(by, label)| MenuRow::Item { key: label, label, hint: None, mark: Mark::Dot(app.sort_by == *by) })
        .collect();
    sub_rows.push(MenuRow::Sep);
    sub_rows.push(MenuRow::Item { key: "asc", label: asc, hint: None, mark: Mark::Dot(!app.sort_desc) });
    sub_rows.push(MenuRow::Item { key: "desc", label: desc, hint: None, mark: Mark::Dot(app.sort_desc) });

    let screen = ctx.screen_rect();
    let size = vec2(W, MENU_PAD * 2.0 + rows.iter().map(menu_row_h).sum::<f32>());
    let rect = match app.menu_at {
        // Opened by a right-click: at the pointer, kept inside the window (opening
        // upwards / leftwards when there's no room below / to the right)
        Some(p) => {
            let x = if p.x + size.x <= screen.right() - 4.0 { p.x } else { p.x - size.x };
            let y = if p.y + size.y <= screen.bottom() - 4.0 { p.y } else { p.y - size.y };
            Rect::from_min_size(
                pos2(
                    x.clamp(screen.left() + 4.0, (screen.right() - 4.0 - size.x).max(screen.left() + 4.0)),
                    y.clamp(screen.top() + 4.0, (screen.bottom() - 4.0 - size.y).max(screen.top() + 4.0)),
                ),
                size,
            )
        }
        None => Rect::from_min_size(pos2(MENU_X - BTN_BOX / 2.0, MENU_Y + BTN_BOX / 2.0 + 4.0), size),
    };
    // The submenu sits beside its row (last frame's position of the row): to the
    // right, else to the left, else over the menu's right side
    let sub_h = MENU_PAD * 2.0 + sub_rows.iter().map(menu_row_h).sum::<f32>();
    let sub_rect = app.menu_sub.map(|(_, row)| {
        let x = if rect.right() - 2.0 + SUB_W <= screen.right() - 4.0 {
            rect.right() - 2.0
        } else if rect.left() + 2.0 - SUB_W >= screen.left() + 4.0 {
            rect.left() + 2.0 - SUB_W
        } else {
            (screen.right() - 4.0 - SUB_W).max(screen.left() + 4.0)
        };
        let y = (row.top() - MENU_PAD).min(screen.bottom() - 4.0 - sub_h).max(screen.top() + 4.0);
        Rect::from_min_size(pos2(x, y), vec2(SUB_W, sub_h))
    });

    // Any press outside the menus (and the menu button, which toggles it) closes it
    let anchor = app.menu_anchor;
    let pressed_outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer.press_origin().is_some_and(|o| {
                !rect.contains(o) && !anchor.contains(o) && !sub_rect.is_some_and(|s| s.contains(o))
            })
    });
    if pressed_outside {
        app.main_menu = false;
        app.menu_sub = None;
        return;
    }

    let id = egui::Id::new("main_menu");
    let sub_open = app.menu_sub.map(|(k, _)| k);
    let mut out = (None, None, Vec::new());
    egui::Area::new(id).order(POPUP).fixed_pos(rect.min).show(ctx, |ui| {
        out = draw_menu_rows(ui, rect, &rows, sub_open, None, MenuSize::NORMAL);
    });
    ctx.move_to_top(egui::LayerId::new(POPUP, id));
    let (mut picked, hovered, subs) = out;

    let mut sub_picked = None;
    if let Some(sr) = sub_rect {
        let sid = egui::Id::new("main_menu_sub");
        egui::Area::new(sid).order(POPUP).fixed_pos(sr.min).show(ctx, |ui| {
            sub_picked = draw_menu_rows(ui, sr, &sub_rows, None, None, MenuSize::NORMAL).0;
        });
        ctx.move_to_top(egui::LayerId::new(POPUP, sid));
    }

    // Submenu opening / closing, with a short delay like Windows' menus (so the
    // pointer can cross other rows on its way into the submenu)
    let now = std::time::Instant::now();
    if hovered != app.menu_hover.map(|(k, _)| k) {
        app.menu_hover = hovered.map(|k| (k, now));
    }
    let rested = app.menu_hover.is_some_and(|(_, t0)| t0.elapsed().as_secs_f32() >= SUBMENU_DELAY);
    if app.menu_hover.is_some() && !rested {
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }
    let row_of = |key: &str| subs.iter().find(|(k, _)| *k == key).map(|(_, r)| *r);
    let was_open = app.menu_sub.is_some();
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    if hovered == Some("sort") {
        app.menu_aim = pointer;
    }
    if picked == Some("sort") {
        // Clicking the row opens / closes it straight away
        app.menu_sub = if app.menu_sub.is_some() { None } else { row_of("sort").map(|r| ("sort", r)) };
        picked = None;
    } else if hovered.is_some_and(|k| k != "sort") {
        // On another row the submenu closes straight away, unless the pointer is
        // still moving across towards it (through the triangle between where it
        // left the row and the submenu's near edge, like Windows' menus)
        let heading_in = match (sub_rect, app.menu_aim, pointer) {
            (Some(s), Some(from), Some(p)) => {
                ctx.input(|i| i.pointer.time_since_last_movement()) < 0.12
                    && in_triangle(p, from - vec2(2.0, 0.0), s.left_top() - vec2(0.0, 4.0), s.left_bottom() + vec2(0.0, 4.0))
            }
            _ => false,
        };
        if heading_in {
            ctx.request_repaint_after(std::time::Duration::from_millis(40));
        } else {
            app.menu_sub = None;
        }
    } else if rested && hovered == Some("sort") && app.menu_sub.is_none() {
        app.menu_sub = row_of("sort").map(|r| ("sort", r));
    }
    // Opened or closed after this frame was laid out: draw again right away
    if app.menu_sub.is_some() != was_open {
        ctx.request_repaint();
    }
    if sub_picked.is_some() {
        picked = sub_picked;
    }

    // Any choice closes the menu
    if picked.is_some() {
        app.main_menu = false;
        app.menu_sub = None;
    }
    match picked {
        Some("open") => app.show_open_dialog(),
        Some("keys") => app.show_keys = true,
        Some("quit") => app.quit(),
        Some("autoplay") => app.autoplay = !app.autoplay,
        Some("open_url") => app.show_url_box(),
        Some("save") => app.ask_save_folder(),
        Some("clip") => app.start_clip(),
        Some("cancel_save") => app.save = None,
        Some("adjust") => {
            app.adjust_ui.open = true;
            app.show_info = false;
            app.adjust_ui.online_open = false;
        }
        Some("online") => {
            app.adjust_ui.online_open = true;
            app.show_info = false;
            app.adjust_ui.open = false;
            app.adjust_ui.eq_open = false;
        }
        Some("multi") => app.multi = !app.multi,
        Some("border") => app.border = !app.border,
        Some("glass") => app.glass = !app.glass,
        Some("right_click") => app.right_click_menu = !app.right_click_menu,
        Some("snap") => {
            app.snap = !app.snap;
            taskbar::set_snap(app.snap);
        }
        Some("asc") => app.set_sort(app.sort_by, false),
        Some("desc") => app.set_sort(app.sort_by, true),
        Some(key) => {
            if let Some((by, _)) = SORT_CHOICES.iter().find(|(_, label)| *label == key) {
                app.set_sort(*by, app.sort_desc);
            }
        }
        None => {}
    }
}

/// Playback speed menu, above its button in the control bar
pub(crate) fn draw_speed_menu(app: &mut FluentMediaPlayer, ctx: &egui::Context) {
    if !app.speed_menu {
        return;
    }
    const W: f32 = 124.0;
    let mut rows = vec![MenuRow::Title("Playback speed"), MenuRow::Sep];
    for (s, label) in SPEEDS {
        rows.push(MenuRow::Item { key: label, label, hint: None, mark: Mark::Dot((app.speed - s).abs() < 1e-6) });
    }
    let h = MENU_PAD * 2.0 + rows.iter().map(|r| MenuSize::COMPACT.row_h(r)).sum::<f32>();
    let screen = ctx.screen_rect();
    let anchor = app.speed_anchor;
    let x = (anchor.center().x + BTN_BOX / 2.0 - W).clamp(screen.left() + 4.0, screen.right() - 4.0 - W);
    let y = (anchor.top() - 4.0 - h).max(screen.top() + 4.0);
    let rect = Rect::from_min_size(pos2(x, y), vec2(W, h));

    // A press outside (other than on its button, which toggles it) closes it
    let pressed_outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer.press_origin().is_some_and(|o| !rect.contains(o) && !anchor.contains(o))
    });
    if pressed_outside {
        app.speed_menu = false;
        return;
    }
    let id = egui::Id::new("speed_menu");
    let mut picked = None;
    let selected = app.accent.gamma_multiply(0.42);
    egui::Area::new(id).order(POPUP).fixed_pos(rect.min).show(ctx, |ui| {
        picked = draw_menu_rows(ui, rect, &rows, None, Some(selected), MenuSize::COMPACT).0;
    });
    ctx.move_to_top(egui::LayerId::new(POPUP, id));
    if let Some(key) = picked {
        if let Some((s, _)) = SPEEDS.iter().find(|(_, l)| *l == key) {
            app.set_speed(*s);
        }
        app.speed_menu = false;
    }
}

/// Decoder / video details (toggle with I) — shows whether hardware decoding is active
/// The info overlay (Tab): a frosted panel like the menus, with sections, bold
/// labels and a value column
pub(crate) fn draw_info_overlay(ctx: &egui::Context, rows: &[InfoRow], alpha: f32) {
    if rows.is_empty() {
        return;
    }
    const PAD_X: f32 = 16.0;
    const PAD_Y: f32 = 10.0;
    const ROW_H: f32 = 20.0;
    const HEAD_H: f32 = 26.0;
    const COL_GAP: f32 = 18.0;
    const FOOT_H: f32 = 30.0;
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("info_osd")));
    let label_font = FontId::new(13.0, FontFamily::Name("semibold".into()));
    let value_font = FontId::proportional(13.0);
    let head_font = FontId::new(11.5, FontFamily::Name("semibold".into()));
    let foot_font = FontId::proportional(12.0);
    let (label_col, value_col) = (Color32::from_rgb(236, 236, 236).gamma_multiply(alpha), Color32::from_rgb(208, 208, 208).gamma_multiply(alpha));
    let dim = Color32::from_rgb(150, 150, 150).gamma_multiply(alpha);
    let width_of = |t: &str, f: &FontId| p.layout_no_wrap(t.to_owned(), f.clone(), Color32::WHITE).size().x;

    let label_w = rows
        .iter()
        .filter_map(|r| if let InfoRow::Item(l, _) = r { Some(width_of(l, &label_font)) } else { None })
        .fold(0.0_f32, f32::max);
    // The value column only ever grows while the panel is open: numbers changing
    // width every refresh would otherwise make the whole panel twitch
    let value_now = rows
        .iter()
        .filter_map(|r| if let InfoRow::Item(_, v) = r { Some(width_of(v, &value_font)) } else { None })
        .fold(0.0_f32, f32::max);
    let id = egui::Id::new("info_value_w");
    let value_w = ctx.data_mut(|d| {
        let w = d.get_temp_mut_or(id, 0.0_f32);
        *w = w.max(value_now);
        *w
    });
    let footer = "Tab to close  \u{b7}  H for the next decoding method";
    let width = (PAD_X * 2.0 + label_w + COL_GAP + value_w).max(width_of(footer, &foot_font) + PAD_X * 2.0).ceil();
    let content_h: f32 = rows
        .iter()
        .enumerate()
        .map(|(i, r)| match r {
            InfoRow::Section(_) if i == 0 => HEAD_H - 6.0,
            InfoRow::Section(_) => HEAD_H,
            InfoRow::Item(..) => ROW_H,
        })
        .sum();
    let screen = ctx.screen_rect();
    // (top left, where Picture & Sound opens: one or the other)
    let rect = Rect::from_min_size(
        pos2(screen.left() + 8.0, screen.top() + adjust::PANEL_TOP),
        vec2(width, PAD_Y * 2.0 + content_h + FOOT_H),
    );
    glass::shadow(&p, rect, 8.0, alpha);
    glass::fill(&p, rect, 8.0, glass::MENU_BG, alpha);
    p.rect_stroke(
        rect.shrink(0.5),
        8.0,
        Stroke::new(1.0_f32, glass::edge(glass::BORDER).gamma_multiply(alpha)),
    );

    let x = rect.left() + PAD_X;
    let vx = x + label_w + COL_GAP;
    let mut y = rect.top() + PAD_Y;
    for (i, row) in rows.iter().enumerate() {
        match row {
            InfoRow::Section(name) => {
                let h = if i == 0 { HEAD_H - 6.0 } else { HEAD_H };
                p.text(pos2(x, y + h - 9.0), Align2::LEFT_CENTER, name.to_uppercase(), head_font.clone(), dim);
                y += h;
            }
            InfoRow::Item(label, value) => {
                let cy = y + ROW_H / 2.0;
                p.text(pos2(x, cy), Align2::LEFT_CENTER, *label, label_font.clone(), label_col);
                p.text(pos2(vx, cy), Align2::LEFT_CENTER, value, value_font.clone(), value_col);
                y += ROW_H;
            }
        }
    }
    // Footer: a hairline, then the keys
    let line_y = y + PAD_Y - 2.0;
    p.line_segment(
        [pos2(rect.left() + 1.0, line_y), pos2(rect.right() - 1.0, line_y)],
        Stroke::new(1.0_f32, glass::BORDER.gamma_multiply(alpha)),
    );
    p.text(pos2(x, (line_y + rect.bottom()) / 2.0), Align2::LEFT_CENTER, footer, foot_font, dim);
}
