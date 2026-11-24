//! Material 3 Expressive components painted on top of egui: shape-morphing
//! buttons, connected button groups, navigation rail, switches, cards, wavy
//! progress, loading indicator, dialogs.

use std::f32::consts::TAU;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Layout, Margin, Mesh, Painter, Pos2,
    Rect, Response, ScrollArea, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2, emath::easing, lerp,
    pos2, text::LayoutJob, text::TextWrapping, vec2,
};

use crate::theme::{self, Colors, shape};

/// Material Symbols used by the app.
pub mod ic {
    use egui_material_icons::icons::*;
    pub const ADD: &str = ICON_ADD.codepoint;
    pub const REMOVE: &str = ICON_REMOVE.codepoint;
    pub const CLOSE: &str = ICON_CLOSE.codepoint;
    pub const CHECK: &str = ICON_CHECK.codepoint;
    pub const CHECK_CIRCLE: &str = ICON_CHECK_CIRCLE.codepoint;
    pub const HOME: &str = ICON_HOME.codepoint;
    pub const SEARCH: &str = ICON_SEARCH.codepoint;
    pub const HISTORY: &str = ICON_HISTORY.codepoint;
    pub const CHANGES: &str = ICON_DIFFERENCE.codepoint;
    pub const STASH: &str = ICON_INVENTORY_2.codepoint;
    pub const FETCH: &str = ICON_CLOUD_SYNC.codepoint;
    pub const PULL: &str = ICON_DOWNLOAD.codepoint;
    pub const PUSH: &str = ICON_UPLOAD.codepoint;
    pub const BRANCH: &str = ICON_ALT_ROUTE.codepoint;
    pub const MERGE: &str = ICON_MERGE.codepoint;
    pub const REBASE: &str = ICON_MEDIATION.codepoint;
    pub const TAG: &str = ICON_SELL.codepoint;
    pub const CLOUD: &str = ICON_CLOUD.codepoint;
    pub const UPSTREAM_GONE: &str = ICON_CLOUD_OFF.codepoint;
    pub const FOLDER: &str = ICON_FOLDER.codepoint;
    pub const FOLDER_OPEN: &str = ICON_FOLDER_OPEN.codepoint;
    pub const NEW_FOLDER: &str = ICON_CREATE_NEW_FOLDER.codepoint;
    pub const TERMINAL: &str = ICON_TERMINAL.codepoint;
    pub const OPEN_IN: &str = ICON_OPEN_IN_NEW.codepoint;
    pub const CODE: &str = ICON_CODE.codepoint;
    pub const WEB: &str = ICON_PUBLIC.codepoint;
    pub const REFRESH: &str = ICON_REFRESH.codepoint;
    pub const COPY: &str = ICON_CONTENT_COPY.codepoint;
    pub const EXPAND: &str = ICON_EXPAND_MORE.codepoint;
    pub const CHEVRON: &str = ICON_CHEVRON_RIGHT.codepoint;
    pub const EDIT: &str = ICON_EDIT.codepoint;
    pub const UNDO: &str = ICON_UNDO.codepoint;
    pub const UP: &str = ICON_KEYBOARD_ARROW_UP.codepoint;
    pub const DOWN: &str = ICON_KEYBOARD_ARROW_DOWN.codepoint;
    pub const DELETE: &str = ICON_DELETE.codepoint;
    pub const DARK: &str = ICON_DARK_MODE.codepoint;
    pub const LIGHT: &str = ICON_LIGHT_MODE.codepoint;
    pub const AUTO: &str = ICON_BRIGHTNESS_AUTO.codepoint;
    pub const PALETTE: &str = ICON_PALETTE.codepoint;
    pub const SETTINGS: &str = ICON_SETTINGS.codepoint;
    pub const TOOLS: &str = ICON_BUILD.codepoint;
    pub const COMPARE: &str = ICON_COMPARE.codepoint;
    pub const RADIO_ON: &str = ICON_RADIO_BUTTON_CHECKED.codepoint;
    pub const RADIO_OFF: &str = ICON_RADIO_BUTTON_UNCHECKED.codepoint;
    pub const WARNING: &str = ICON_WARNING.codepoint;
    pub const ERROR: &str = ICON_ERROR.codepoint;
    pub const FILE: &str = ICON_DESCRIPTION.codepoint;
    pub const WORKTREE: &str = ICON_ACCOUNT_TREE.codepoint;
    pub const SUBMODULE: &str = ICON_EXTENSION.codepoint;
    pub const COMMIT: &str = ICON_COMMIT.codepoint;
    pub const PICK: &str = ICON_COLORIZE.codepoint;
    pub const RESET: &str = ICON_RESTART_ALT.codepoint;
    pub const PLAY: &str = ICON_PLAY_ARROW.codepoint;
    pub const SKIP: &str = ICON_SKIP_NEXT.codepoint;
    pub const CANCEL: &str = ICON_CANCEL.codepoint;
    pub const STAGE_ALL: &str = ICON_DONE_ALL.codepoint;
    pub const CONTRAST: &str = ICON_CONTRAST.codepoint;
    pub const PERSON: &str = ICON_PERSON.codepoint;
    pub const LINK: &str = ICON_LINK.codepoint;
    pub const FILTER: &str = ICON_FILTER_LIST.codepoint;
}

// ---------------------------------------------------------------- basics

pub fn colors(ui: &Ui) -> Colors {
    theme::colors(ui.ctx())
}

pub fn radius(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().clamp(0.0, 255.0) as u8)
}

/// Child UI pinned to `rect`, clipped to it, with a stable id.
pub fn child<R>(ui: &mut Ui, salt: &str, rect: Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    // `new_child` (not `scope_builder`): hand-placed panes must not move the parent's cursor.
    let mut child = ui.new_child(UiBuilder::new().id_salt(salt).max_rect(rect).layout(Layout::top_down(Align::Min)));
    child.shrink_clip_rect(rect);
    add(&mut child)
}

pub fn card(ui: &Ui, rect: Rect, fill: Color32, r: f32) {
    ui.painter().rect_filled(rect, radius(r), fill);
}

pub fn icon_at(p: &Painter, center: Pos2, icon: &str, size: f32, color: Color32) {
    // Centre the drawn glyph, not the font's line box: the two differ by a pixel or two,
    // which shows in small containers such as the switch thumb.
    let galley = p.layout_no_wrap(icon.to_owned(), theme::icon(size), color);
    let pos = center - galley.mesh_bounds.center().to_vec2();
    p.galley(pos, galley, color);
}

/// One line of text, vertically centred in `rect` and truncated with an ellipsis.
/// Returns the width used.
pub fn label_in(p: &Painter, rect: Rect, text: &str, font: FontId, color: Color32) -> f32 {
    if rect.width() <= 4.0 {
        return 0.0;
    }
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(rect.width());
    let galley = p.layout_job(job);
    let size = galley.size();
    p.galley(pos2(rect.left(), rect.center().y - size.y / 2.0), galley, color);
    size.x
}

pub fn text_width(p: &Painter, text: &str, font: FontId) -> f32 {
    p.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER).size().x
}

/// (hover, press) animation progress of a widget.
fn interaction(ui: &Ui, r: &Response) -> (f32, f32) {
    let ctx = ui.ctx();
    (
        ctx.animate_bool_with_time(r.id.with("hover"), r.hovered(), 0.12),
        ctx.animate_bool_with_time(r.id.with("press"), r.is_pointer_button_down_on(), 0.10),
    )
}

/// Container with state layer; corners tighten while pressed (expressive shape morph).
fn morph_body(ui: &Ui, rect: Rect, r: &Response, fill: Color32, content: Color32, rest_radius: f32) {
    let (hover, press) = interaction(ui, r);
    let corner = radius(lerp(rest_radius..=rest_radius.min(shape::SMALL + 2.0), press));
    let p = ui.painter();
    p.rect_filled(rect, corner, fill);
    p.rect_filled(rect, corner, content.gamma_multiply(0.08 * hover + 0.08 * press));
}

// ---------------------------------------------------------------- buttons

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Filled,
    Tonal,
    Outlined,
    Text,
    Danger,
    /// Text button on an inverse surface (snackbar).
    Inverse,
    /// Icon-only buttons: no container until hovered.
    Standard,
}

fn kind_colors(c: &Colors, kind: Kind) -> (Color32, Color32) {
    match kind {
        Kind::Filled => (c.primary, c.on_primary),
        Kind::Tonal => (c.secondary_container, c.on_secondary_container),
        Kind::Outlined | Kind::Text => (Color32::TRANSPARENT, c.primary),
        Kind::Danger => (c.error, c.on_error),
        Kind::Inverse => (Color32::TRANSPARENT, c.inverse_primary),
        Kind::Standard => (Color32::TRANSPARENT, c.on_surface_variant),
    }
}

pub struct Btn<'a> {
    label: &'a str,
    icon: Option<&'a str>,
    kind: Kind,
    height: f32,
    enabled: bool,
}

pub fn btn(label: &str) -> Btn<'_> {
    Btn { label, icon: None, kind: Kind::Filled, height: 40.0, enabled: true }
}

impl<'a> Btn<'a> {
    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let c = colors(ui);
        let font = theme::emph(if self.height < 36.0 { 13.0 } else { 14.0 });
        let label_w = text_width(ui.painter(), self.label, font.clone());
        let pad = if self.kind == Kind::Text || self.kind == Kind::Inverse { 12.0 } else { self.height * 0.5 };
        let icon_w = if self.icon.is_some() { 18.0 + 8.0 } else { 0.0 };
        let lead = if self.icon.is_some() { pad - 4.0 } else { pad };
        let size = vec2(lead + icon_w + label_w + pad, self.height);
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(size, sense);
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let (mut fill, mut fg) = kind_colors(&c, self.kind);
        if !self.enabled {
            fg = c.on_surface.gamma_multiply(0.38);
            fill = if fill == Color32::TRANSPARENT { fill } else { c.on_surface.gamma_multiply(0.12) };
        }
        morph_body(ui, rect, &resp, fill, fg, self.height / 2.0);
        let p = ui.painter();
        if self.kind == Kind::Outlined {
            let (_, press) = interaction(ui, &resp);
            let corner = radius(lerp(self.height / 2.0..=shape::SMALL + 2.0, press));
            p.rect_stroke(rect, corner, Stroke::new(1.0, c.outline_variant), StrokeKind::Inside);
        }
        let mut x = rect.left() + lead;
        if let Some(icon) = self.icon {
            icon_at(p, pos2(x + 9.0, rect.center().y), icon, 18.0, fg);
            x += icon_w;
        }
        p.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, self.label, font, fg);
        if self.enabled {
            resp.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            resp
        }
    }
}

pub struct IconBtn<'a> {
    icon: &'a str,
    tip: &'a str,
    kind: Kind,
    size: f32,
    selected: bool,
    enabled: bool,
}

pub fn icon_btn<'a>(icon: &'a str, tip: &'a str) -> IconBtn<'a> {
    IconBtn { icon, tip, kind: Kind::Standard, size: 40.0, selected: false, enabled: true }
}

impl IconBtn<'_> {
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }
    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
    /// Toggle state: selected buttons take a squarer shape and a tonal fill.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(self.size), sense);
        self.paint(ui, rect, resp)
    }

    /// For buttons placed by hand (row actions).
    pub fn show_at(self, ui: &Ui, id: Id, rect: Rect) -> Response {
        let resp = ui.interact(rect, id, Sense::click());
        self.paint(ui, rect, resp)
    }

    fn paint(self, ui: &Ui, rect: Rect, resp: Response) -> Response {
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let c = colors(ui);
        let (mut fill, mut fg) = kind_colors(&c, self.kind);
        if self.selected {
            (fill, fg) = (c.secondary_container, c.on_secondary_container);
        }
        if !self.enabled {
            fg = c.on_surface.gamma_multiply(0.38);
        }
        let square = ui.ctx().animate_bool_with_time(resp.id.with("selected"), self.selected, 0.2);
        let rest = lerp(self.size / 2.0..=self.size * 0.3, square);
        morph_body(ui, rect, &resp, fill, fg, rest);
        icon_at(ui.painter(), rect.center(), self.icon, self.size * 0.5, fg);
        let resp = if self.enabled { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp };
        if self.tip.is_empty() { resp } else { resp.on_hover_text(self.tip) }
    }
}

/// Connected button group: neighbours share tight inner corners, the selected
/// option rounds out fully and takes the primary fill.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, salt: &str, value: &mut T, options: &[(T, Option<&str>, &str)], height: f32) -> bool {
    let c = colors(ui);
    let font = theme::emph(13.0);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, icon, label)| {
            let icon_w = if icon.is_some() { 18.0 + if label.is_empty() { 0.0 } else { 6.0 } } else { 0.0 };
            28.0 + icon_w + text_width(ui.painter(), label, font.clone())
        })
        .collect();
    let gap = 2.0;
    let total = widths.iter().sum::<f32>() + gap * (options.len().saturating_sub(1)) as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(total, height), Sense::hover());
    let base = ui.scope_id().with(salt);
    let mut changed = false;
    let mut x = rect.left();
    for (i, ((option, icon, label), w)) in options.iter().zip(&widths).enumerate() {
        let item = Rect::from_min_size(pos2(x, rect.top()), vec2(*w, height));
        x += w + gap;
        let resp = ui.interact(item, base.with(i), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let selected = *value == *option;
        if resp.clicked() && !selected {
            *value = *option;
            changed = true;
        }
        let t = ui.ctx().animate_bool_with_time(resp.id.with("selected"), selected, 0.2);
        let (hover, press) = interaction(ui, &resp);
        let full = height / 2.0;
        let inner = lerp(6.0..=full, t) - 2.0 * press;
        let (first, last) = (i == 0, i + 1 == options.len());
        let corner = |outer: bool| (if outer { full } else { inner }).clamp(2.0, 255.0) as u8;
        let corners = CornerRadius { nw: corner(first), sw: corner(first), ne: corner(last), se: corner(last) };
        let fill = c.surface_highest.lerp_to_gamma(c.primary, t);
        let fg = c.on_surface_variant.lerp_to_gamma(c.on_primary, t);
        let p = ui.painter();
        p.rect_filled(item, corners, fill);
        p.rect_filled(item, corners, fg.gamma_multiply(0.08 * hover + 0.08 * press));
        let mut tx = item.left() + 14.0;
        if let Some(icon) = icon {
            icon_at(p, pos2(tx + 9.0, item.center().y), icon, 18.0, fg);
            tx += 24.0;
        }
        p.text(pos2(tx, item.center().y), Align2::LEFT_CENTER, label, font.clone(), fg);
    }
    changed
}

/// Navigation rail destination: pill indicator springs open when selected.
pub fn nav_item(ui: &mut Ui, icon: &str, label: &str, selected: bool, badge: usize) -> Response {
    let c = colors(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(72.0, 60.0), Sense::click());
    let t = ui.ctx().animate_bool_with_time_and_easing(resp.id.with("selected"), selected, 0.35, easing::back_out);
    let (hover, press) = interaction(ui, &resp);
    let center = pos2(rect.center().x, rect.top() + 20.0);
    let p = ui.painter();
    let pill = |w: f32| Rect::from_center_size(center, vec2(w, 32.0));
    p.rect_filled(pill(56.0), radius(16.0), c.on_surface.gamma_multiply(0.08 * hover * (1.0 - t.clamp(0.0, 1.0))));
    if t > 0.01 {
        let w = lerp(28.0..=56.0, t) - 4.0 * press;
        p.rect_filled(pill(w), radius(16.0), c.secondary_container.gamma_multiply(t.clamp(0.0, 1.0)));
    }
    let fg = if selected { c.on_secondary_container } else { c.on_surface_variant };
    icon_at(p, center, icon, 22.0, fg);
    let (font, text_color) = if selected { (theme::emph(11.5), c.on_surface) } else { (theme::body(11.5), c.on_surface_variant) };
    p.text(pos2(rect.center().x, rect.top() + 47.0), Align2::CENTER_CENTER, label, font, text_color);
    if badge > 0 {
        let text = if badge > 99 { "99+".to_string() } else { badge.to_string() };
        let w = (text_width(p, &text, theme::emph(10.0)) + 8.0).max(16.0);
        let badge_rect = Rect::from_center_size(center + vec2(13.0, -9.0), vec2(w, 16.0));
        p.rect_filled(badge_rect, radius(8.0), c.primary);
        p.text(badge_rect.center(), Align2::CENTER_CENTER, text, theme::emph(10.0), c.on_primary);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

// ---------------------------------------------------------------- lists

/// Selection pill / hover layer behind a list row.
pub fn row_bg(ui: &Ui, rect: Rect, resp: &Response, selected: bool) {
    let c = colors(ui);
    let r = radius((rect.height() / 2.0).min(shape::LARGE));
    let area = rect.shrink2(vec2(0.0, 1.0));
    let p = ui.painter();
    if selected {
        p.rect_filled(area, r, c.secondary_container);
    }
    let (hover, press) = interaction(ui, resp);
    if hover > 0.0 || press > 0.0 {
        p.rect_filled(area, r, c.on_surface.gamma_multiply(0.06 * hover + 0.06 * press));
    }
}

/// Virtualised list of fixed-height rows inside `rect`. `body` gets the visible
/// row range and adds each row with [`row`]. `scroll_to` centres that row.
pub fn rows(ui: &mut Ui, salt: &str, rect: Rect, row_height: f32, count: usize, scroll_to: Option<usize>, body: impl FnOnce(&mut Ui, std::ops::Range<usize>)) {
    child(ui, salt, rect, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = ScrollArea::vertical().id_salt(salt).auto_shrink(false);
        if let Some(index) = scroll_to {
            area = area.vertical_scroll_offset((index as f32 * row_height - rect.height() / 2.0).max(0.0));
        }
        area.show_rows(ui, row_height, count, body);
    });
}

/// One full-width clickable row of a [`rows`] list.
pub fn row(ui: &mut Ui, height: f32) -> (Rect, Response) {
    ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click())
}

/// Child UI laid out left to right, vertically centred.
pub fn hchild<R>(ui: &mut Ui, salt: &str, rect: Rect, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut child = ui.new_child(UiBuilder::new().id_salt(salt).max_rect(rect).layout(Layout::left_to_right(Align::Center)));
    child.shrink_clip_rect(rect);
    add(&mut child)
}

/// List section header, collapsible when `open` is given. Returns true when the trailing action was clicked.
pub fn section_header(ui: &mut Ui, title: &str, count: usize, open: Option<&mut bool>, action: Option<(&str, &str)>) -> bool {
    let c = colors(ui);
    let sense = if open.is_some() { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), sense);
    let mid = rect.center().y;
    let mut text_x = rect.left() + 10.0;
    if let Some(open) = open {
        if resp.clicked() {
            *open = !*open;
        }
        row_bg(ui, rect, &resp, false);
        icon_at(ui.painter(), pos2(rect.left() + 14.0, mid), if *open { ic::EXPAND } else { ic::CHEVRON }, 18.0, c.on_surface_variant);
        text_x = rect.left() + 28.0;
    }
    let p = ui.painter();
    let end = p.text(pos2(text_x, mid), Align2::LEFT_CENTER, title, theme::emph(13.0), c.on_surface_variant).right();
    let text = count.to_string();
    let w = text_width(p, &text, theme::emph(11.0)) + 12.0;
    let pill = Rect::from_min_size(pos2(end + 8.0, mid - 9.0), vec2(w.max(20.0), 18.0));
    p.rect_filled(pill, radius(9.0), c.surface_highest);
    p.text(pill.center(), Align2::CENTER_CENTER, text, theme::emph(11.0), c.on_surface_variant);
    let Some((icon, tip)) = action else { return false };
    let button = Rect::from_center_size(pos2(rect.right() - 18.0, mid), Vec2::splat(28.0));
    icon_btn(icon, tip).size(28.0).show_at(ui, resp.id.with("action"), button).clicked()
}

/// Small rounded label (branch, tag, remote ref). Returns its width.
pub fn chip(p: &Painter, left_center: Pos2, icon: &str, text: &str, fill: Color32, fg: Color32) -> f32 {
    chip_with(p, left_center, icon, text, "", fill, fg)
}

/// A chip with a second icon after its text; `trail` is empty for none. `icon` may be empty too.
pub fn chip_with(p: &Painter, left_center: Pos2, icon: &str, text: &str, trail: &str, fill: Color32, fg: Color32) -> f32 {
    let font = theme::emph(11.5);
    let text_w = text_width(p, text, font.clone());
    let lead = if icon.is_empty() { 8.0 } else { 22.0 };
    let w = lead + text_w + 8.0 + if trail.is_empty() { 0.0 } else { 15.0 };
    let rect = Rect::from_min_size(left_center - vec2(0.0, 10.0), vec2(w, 20.0));
    p.rect_filled(rect, radius(shape::SMALL), fill);
    if !icon.is_empty() {
        icon_at(p, pos2(rect.left() + 12.0, rect.center().y), icon, 13.0, fg);
    }
    p.text(pos2(rect.left() + lead, rect.center().y), Align2::LEFT_CENTER, text, font, fg);
    if !trail.is_empty() {
        icon_at(p, pos2(rect.right() - 12.0, rect.center().y), trail, 12.0, fg);
    }
    w
}

/// Git status letter in a tonal square.
pub fn status_badge(p: &Painter, center: Pos2, status: char, c: &Colors) {
    let (fill, fg) = match status {
        'A' | '?' => (c.diff_added, c.added),
        'D' => (c.diff_deleted, c.deleted),
        'U' => (c.error, c.on_error),
        'R' | 'C' => (c.secondary_container, c.on_secondary_container),
        _ => (c.tertiary_container, c.on_tertiary_container),
    };
    p.rect_filled(Rect::from_center_size(center, Vec2::splat(18.0)), radius(6.0), fill);
    p.text(center, Align2::CENTER_CENTER, status, theme::emph(11.0), fg);
}

pub fn avatar(p: &Painter, center: Pos2, r: f32, name: &str, c: &Colors) {
    let hash = name.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    let initials: String = name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
    p.circle_filled(center, r, c.lanes[hash % c.lanes.len()]);
    p.text(center, Align2::CENTER_CENTER, initials, theme::emph(r * 0.8), c.surface);
}

// ---------------------------------------------------------------- inputs

/// Pill-shaped single-line field.
pub fn text_field(ui: &mut Ui, text: &mut String, hint: &str, icon: Option<&str>, width: f32) -> Response {
    let c = colors(ui);
    let height = 40.0;
    let (rect, outer) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let background = ui.painter().add(Shape::Noop);
    let mut inner = rect.shrink2(vec2(16.0, 0.0));
    if let Some(icon) = icon {
        icon_at(ui.painter(), pos2(inner.left() + 8.0, rect.center().y), icon, 18.0, c.on_surface_variant);
        inner.min.x += 26.0;
    }
    let mut field = ui.new_child(UiBuilder::new().id_salt(outer.id).max_rect(inner).layout(Layout::left_to_right(Align::Center)));
    let resp = field.add(TextEdit::singleline(text).frame(Frame::NONE).hint_text(hint).desired_width(inner.width()).font(theme::body(14.0)));
    let stroke = if resp.has_focus() { Stroke::new(2.0, c.primary) } else { Stroke::NONE };
    let shape = egui::epaint::RectShape::new(rect, radius(height / 2.0), c.surface_highest, stroke, StrokeKind::Inside);
    ui.painter().set(background, shape);
    resp
}

/// Multi-line field filling `rect`.
pub fn text_area(ui: &mut Ui, salt: &str, rect: Rect, text: &mut String, hint: &str) -> Response {
    let c = colors(ui);
    let background = ui.painter().add(Shape::Noop);
    let resp = child(ui, salt, rect.shrink2(vec2(14.0, 10.0)), |ui| {
        ScrollArea::vertical()
            .id_salt(salt)
            .auto_shrink(false)
            .show(ui, |ui| {
                let edit = TextEdit::multiline(text).frame(Frame::NONE).hint_text(hint).desired_width(f32::INFINITY).desired_rows(3).font(theme::body(14.0));
                ui.add(edit)
            })
            .inner
    });
    let stroke = if resp.has_focus() { Stroke::new(2.0, c.primary) } else { Stroke::NONE };
    let shape = egui::epaint::RectShape::new(rect, radius(shape::LARGE), c.surface_highest, stroke, StrokeKind::Inside);
    ui.painter().set(background, shape);
    resp
}

#[derive(Clone, Copy, PartialEq)]
pub enum Check {
    Off,
    On,
    /// Some of a group.
    Mixed,
}

/// M3 checkbox centred at `center`, in the given accent color. The caller applies the click.
pub fn checkbox_at(ui: &Ui, id: Id, center: Pos2, state: Check, accent: Color32, on_accent: Color32) -> Response {
    let c = colors(ui);
    let resp = ui.interact(Rect::from_center_size(center, Vec2::splat(30.0)), id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let (hover, press) = interaction(ui, &resp);
    let filled = ui.ctx().animate_bool_with_time(id.with("checked"), state != Check::Off, 0.12);
    let p = ui.painter();
    p.circle_filled(center, 15.0, accent.gamma_multiply(0.10 * hover + 0.10 * press));
    let square = Rect::from_center_size(center, Vec2::splat(18.0));
    p.rect_stroke(square, radius(4.0), Stroke::new(2.0, c.on_surface_variant.gamma_multiply(1.0 - filled)), StrokeKind::Inside);
    p.rect_filled(square, radius(4.0), accent.gamma_multiply(filled));
    if state != Check::Off {
        icon_at(p, center, if state == Check::On { ic::CHECK } else { ic::REMOVE }, 15.0, on_accent.gamma_multiply(filled));
    }
    resp
}

/// M3 switch with a label; the thumb grows and shows a check when on.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let c = colors(ui);
    let label_w = text_width(ui.painter(), label, theme::body(14.0));
    let (rect, mut resp) = ui.allocate_exact_size(vec2(44.0 + 10.0 + label_w, 28.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(resp.id.with("on"), *on, 0.18);
    let (hover, press) = interaction(ui, &resp);
    let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - 13.0), vec2(44.0, 26.0));
    let p = ui.painter();
    p.rect_filled(track, radius(13.0), c.surface_highest.lerp_to_gamma(c.primary, t));
    if t < 1.0 {
        p.rect_stroke(track, radius(13.0), Stroke::new(2.0, c.outline.gamma_multiply(1.0 - t)), StrokeKind::Inside);
    }
    let thumb = pos2(lerp(track.left() + 13.0..=track.right() - 13.0, t), track.center().y);
    let thumb_r = lerp(6.5..=9.5, t) + hover + 1.5 * press;
    p.circle_filled(thumb, thumb_r, c.outline.lerp_to_gamma(c.on_primary, t));
    if t > 0.5 {
        icon_at(p, thumb, ic::CHECK, 13.0, c.primary.gamma_multiply((t - 0.5) * 2.0));
    }
    p.text(pos2(track.right() + 10.0, rect.center().y), Align2::LEFT_CENTER, label, theme::body(14.0), c.on_surface);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Drag handle in the gap between two panes. Returns the drag distance.
pub fn splitter(ui: &Ui, id: Id, gap: Rect, horizontal_drag: bool) -> f32 {
    let c = colors(ui);
    let resp = ui.interact(gap, id, Sense::drag());
    let active = resp.hovered() || resp.dragged();
    let t = ui.ctx().animate_bool_with_time(id.with("active"), active, 0.15);
    let (long, thick) = (lerp(28.0..=44.0, t), 4.0);
    let size = if horizontal_drag { vec2(thick, long) } else { vec2(long, thick) };
    let color = c.outline_variant.lerp_to_gamma(c.primary, t);
    ui.painter().rect_filled(Rect::from_center_size(gap.center(), size), radius(2.0), color);
    if active {
        ui.ctx().set_cursor_icon(if horizontal_drag { CursorIcon::ResizeHorizontal } else { CursorIcon::ResizeVertical });
    }
    if horizontal_drag { resp.drag_delta().x } else { resp.drag_delta().y }
}

// ---------------------------------------------------------------- menus

const MENU_WIDTH: f32 = 280.0;

fn menu_row(ui: &mut Ui, icon: &str, label: &str, fg: Color32) -> bool {
    ui.spacing_mut().item_spacing.y = 0.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(MENU_WIDTH, 36.0), Sense::click());
    let (hover, press) = interaction(ui, &resp);
    let p = ui.painter();
    p.rect_filled(rect, radius(shape::MEDIUM), fg.gamma_multiply(0.08 * hover + 0.08 * press));
    icon_at(p, pos2(rect.left() + 18.0, rect.center().y), icon, 18.0, fg);
    p.text(pos2(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, label, theme::body(14.0), fg);
    let clicked = resp.clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// The folder hues as a grid of swatches; `current` wears a ring and a check. A swatch
/// swells under the pointer. Returns the hue clicked, and the hue under the pointer.
pub fn hue_swatches(ui: &mut Ui, current: Option<f32>) -> (Option<f32>, Option<f32>) {
    // As wide as a menu's rows, so a menu keeps its width with them or without.
    const COLUMNS: usize = 6;
    const CELL: Vec2 = vec2(MENU_WIDTH / COLUMNS as f32, 40.0);
    let c = colors(ui);
    let rows = theme::FOLDER_HUES.len().div_ceil(COLUMNS);
    let (grid, _) = ui.allocate_exact_size(vec2(MENU_WIDTH, CELL.y * rows as f32), Sense::hover());
    let (mut clicked, mut hovered) = (None, None);
    for (n, (name, hue)) in theme::FOLDER_HUES.iter().enumerate() {
        let cell = Rect::from_min_size(grid.min + vec2((n % COLUMNS) as f32 * CELL.x, (n / COLUMNS) as f32 * CELL.y), CELL);
        let resp = ui.interact(cell, ui.scope_id().with(("hue", n)), Sense::click());
        let (hover, press) = interaction(ui, &resp);
        let chosen = ui.ctx().animate_bool_with_time(resp.id.with("chosen"), current == Some(*hue), 0.15);
        let (accent, _, _) = c.tint(*hue);
        let p = ui.painter();
        p.circle_filled(cell.center(), 12.0 + 2.5 * hover * (1.0 - chosen) - 1.5 * chosen - press, accent);
        if chosen > 0.0 {
            p.circle_stroke(cell.center(), 15.5, Stroke::new(2.0 * chosen, accent));
            icon_at(p, cell.center(), ic::CHECK, 14.0, c.surface.gamma_multiply(chosen));
        }
        if resp.hovered() {
            hovered = Some(*hue);
        }
        if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(*name).clicked() {
            clicked = Some(*hue);
        }
    }
    (clicked, hovered)
}

pub fn menu_item(ui: &mut Ui, icon: &str, label: &str) -> bool {
    let c = colors(ui);
    menu_row(ui, icon, label, c.on_surface)
}

pub fn menu_danger(ui: &mut Ui, icon: &str, label: &str) -> bool {
    let c = colors(ui);
    menu_row(ui, icon, label, c.error)
}

pub fn menu_separator(ui: &mut Ui) {
    let c = colors(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(MENU_WIDTH, 9.0), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, c.outline_variant));
}

// ---------------------------------------------------------------- expressive shapes

/// Scalloped "cookie" outline around `center`.
fn cookie_points(center: Pos2, r: f32, lobes: f32, depth: f32, rotation: f32) -> Vec<Pos2> {
    const STEPS: usize = 96;
    (0..STEPS)
        .map(|i| {
            let a = i as f32 / STEPS as f32 * TAU;
            let rr = r * (1.0 - depth + depth * (lobes * a).cos());
            center + rr * Vec2::angled(a + rotation)
        })
        .collect()
}

pub fn cookie(p: &Painter, center: Pos2, r: f32, lobes: f32, depth: f32, rotation: f32, fill: Color32) {
    let points = cookie_points(center, r, lobes, depth, rotation);
    // Triangle fan: the shape is star-convex around its centre.
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, fill);
    for point in &points {
        mesh.colored_vertex(*point, fill);
    }
    let n = points.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    p.add(Shape::mesh(mesh));
    p.add(Shape::closed_line(points, Stroke::new(1.5, fill))); // anti-aliased edge
}

/// Expressive loading indicator: a rotating shape that morphs between lobe counts.
pub fn loading_indicator(ui: &Ui, center: Pos2, r: f32) {
    let c = colors(ui);
    let time = ui.input(|i| i.time) as f32;
    let period = 0.9;
    let phase = (time / period).fract();
    let lobes = [4.0, 6.0, 5.0, 8.0][(time / period) as usize % 4];
    let depth = 0.13 * (phase * std::f32::consts::PI).sin();
    cookie(ui.painter(), center, r, lobes, depth, time * 2.4, c.primary);
    ui.ctx().request_repaint();
}

/// Indeterminate wavy linear progress.
pub fn wavy_progress(ui: &Ui, rect: Rect) {
    let c = colors(ui);
    let time = ui.input(|i| i.time) as f32;
    let y = rect.center().y;
    let p = ui.painter();
    p.hline(rect.x_range(), y, Stroke::new(4.0, c.secondary_container));
    let head = (time * 0.6).fract() * 1.4 - 0.2;
    let (from, to) = ((head - 0.35).clamp(0.0, 1.0), head.clamp(0.0, 1.0));
    let (x0, x1) = (rect.left() + from * rect.width(), rect.left() + to * rect.width());
    let points: Vec<Pos2> = (0..=((x1 - x0) / 2.0) as usize)
        .map(|i| {
            let x = x0 + i as f32 * 2.0;
            pos2(x, y + 3.0 * ((x / 22.0 - time * 6.0).sin()))
        })
        .collect();
    if points.len() > 1 {
        p.add(Shape::line(points, Stroke::new(4.0, c.primary)));
    }
    ui.ctx().request_repaint();
}

/// Centred icon-in-a-shape with a title and a hint, for empty panes.
pub fn empty_state(ui: &Ui, rect: Rect, icon: &str, title: &str, hint: &str) {
    let c = colors(ui);
    let p = ui.painter().with_clip_rect(rect);
    let center = rect.center() - vec2(0.0, 30.0);
    cookie(&p, center, 44.0, 8.0, 0.07, 0.0, c.primary_container);
    icon_at(&p, center, icon, 38.0, c.on_primary_container);
    p.text(center + vec2(0.0, 66.0), Align2::CENTER_CENTER, title, theme::emph(16.0), c.on_surface);
    p.text(center + vec2(0.0, 90.0), Align2::CENTER_CENTER, hint, theme::body(13.0), c.on_surface_variant);
}

// ---------------------------------------------------------------- dialogs

/// Modal dialog shell. Returns the body's value and whether the user dismissed it
/// (Escape or click outside).
pub fn dialog<R>(ctx: &egui::Context, id: &str, icon: &str, title: &str, width: f32, body: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let c = theme::colors(ctx);
    let frame = Frame::new()
        .fill(c.surface_high)
        .corner_radius(radius(shape::EXTRA_LARGE))
        .inner_margin(Margin::same(24))
        .shadow(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(70) });
    let modal = egui::Modal::new(Id::unique(id)).frame(frame).backdrop_color(c.scrim.gamma_multiply(0.5));
    let resp = modal.show(ctx, |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        let (head, _) = ui.allocate_exact_size(vec2(width, 76.0), Sense::hover());
        let p = ui.painter();
        icon_at(p, pos2(head.center().x, head.top() + 14.0), icon, 26.0, c.secondary);
        p.text(pos2(head.center().x, head.top() + 52.0), Align2::CENTER_CENTER, title, theme::emph(22.0), c.on_surface);
        ui.add_space(8.0);
        body(ui)
    });
    let dismissed = resp.should_close();
    (resp.inner, dismissed)
}

pub fn field_label(ui: &mut Ui, text: &str) {
    let c = colors(ui);
    ui.add_space(12.0);
    ui.label(egui::RichText::new(text).font(theme::emph(12.0)).color(c.on_surface_variant));
    ui.add_space(6.0);
}

pub fn body_text(ui: &mut Ui, text: &str) {
    let c = colors(ui);
    ui.label(egui::RichText::new(text).font(theme::body(14.0)).color(c.on_surface_variant));
}

/// Right-aligned Cancel / confirm row. `Some(true)` = confirmed, `Some(false)` = cancelled.
pub fn dialog_actions(ui: &mut Ui, confirm: &str, kind: Kind, enabled: bool) -> Option<bool> {
    ui.add_space(24.0);
    let mut result = None;
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if btn(confirm).kind(kind).enabled(enabled).show(ui).clicked() {
            result = Some(true);
        }
        if btn("Cancel").kind(Kind::Text).show(ui).clicked() {
            result = Some(false);
        }
    });
    result
}

/// Message bar at the bottom of the window. Returns (action clicked, dismissed).
pub fn snackbar(ctx: &egui::Context, id: &str, text: &str, error: bool, action: &str) -> (bool, bool) {
    let c = theme::colors(ctx);
    let mut result = (false, false);
    let area = egui::Area::new(Id::unique(id)).order(egui::Order::Foreground).anchor(Align2::CENTER_BOTTOM, vec2(0.0, -20.0));
    area.show(ctx, |ui| {
        let frame = Frame::new().fill(c.inverse_surface).corner_radius(radius(shape::LARGE)).inner_margin(Margin::symmetric(16, 6));
        frame.show(ui, |ui| {
            ui.allocate_ui_with_layout(vec2(0.0, 36.0), Layout::left_to_right(Align::Center), |ui| {
                ui.set_max_width(640.0);
                if error {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                    icon_at(ui.painter(), rect.center(), ic::ERROR, 20.0, c.error_container);
                }
                let label = egui::RichText::new(text).font(theme::body(14.0)).color(c.inverse_on_surface);
                ui.add(egui::Label::new(label).wrap());
                if !action.is_empty() {
                    result.0 = btn(action).kind(Kind::Inverse).height(36.0).show(ui).clicked();
                }
                result.1 = btn("Dismiss").kind(Kind::Inverse).height(36.0).show(ui).clicked();
            });
        });
    });
    result
}
