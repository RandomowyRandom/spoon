//! Conflict editor, shown in a window of its own. Both versions of the file sit side by side and scroll together;
//! conflicting lines are picked there, one by one or a side at a time. The merged
//! result is underneath and can be edited.

use std::path::Path;

use eframe::egui::{Align2, CursorIcon, Frame, Id, Key, Layout, Margin, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit, Ui, Vec2, pos2, vec2};
use eframe::egui::{Align, Painter};

use crate::conflict::{self, Conflict, Document, Segment, Side, Take};
use crate::git::ConflictSide;
use crate::theme::{self, Colors, shape};
use crate::widgets::{self as w, Check, Kind, ic};

const LINE: f32 = 20.0;
const HEADER: f32 = 56.0;
/// Seconds the "Discard changes?" confirmation stays armed.
const DISCARD_WINDOW: f64 = 4.0;

pub enum Action {
    Stay,
    Close,
    /// Write this text and mark the file resolved.
    Save(String),
}

/// One row of the side-by-side view.
#[derive(Clone, Copy)]
enum Row {
    /// A line both sides share.
    Plain { segment: usize, line: usize },
    /// Row `line` of conflict `number`; a side with fewer lines shows a filler.
    Conflict { segment: usize, number: usize, line: usize },
}

struct RowInfo {
    row: Row,
    /// File line numbers of this row in each version.
    current_no: usize,
    incoming_no: usize,
}

/// What the user did in the side-by-side view this frame.
enum Click {
    Line { number: usize, side: Side, line: usize, shift: bool },
    WholeSide { number: usize, side: Side },
}

pub struct MergeEditor {
    pub path: String,
    /// The file as it was opened. Saving refuses if the file changed since.
    pub original: String,
    doc: Document,
    rows: Vec<RowInfo>,
    /// First row of each conflict.
    starts: Vec<usize>,
    sides: Option<(ConflictSide, ConflictSide)>,
    /// Conflict the previous / next buttons move from.
    focus: usize,
    scroll_rows: bool,
    scroll_result: bool,
    /// Last line clicked, where a Shift-click range starts: conflict, side, line.
    anchor: Option<(usize, Side, usize)>,
    /// When "Cancel" was first pressed with unsaved work.
    discard_armed: Option<f64>,
    /// Share of the height given to the side-by-side view.
    split: f32,
}

fn build_rows(doc: &Document) -> (Vec<RowInfo>, Vec<usize>) {
    let (mut rows, mut starts) = (Vec::new(), Vec::new());
    let (mut current_no, mut incoming_no) = (1, 1);
    for (segment, content) in doc.segments.iter().enumerate() {
        let (count, current, incoming) = match content {
            Segment::Plain(lines) => (lines.len(), lines.len(), lines.len()),
            Segment::Conflict(c) => (c.current.len().max(c.incoming.len()).max(1), c.current.len(), c.incoming.len()),
        };
        if matches!(content, Segment::Conflict(_)) {
            starts.push(rows.len());
        }
        for line in 0..count {
            let row = match content {
                Segment::Plain(_) => Row::Plain { segment, line },
                Segment::Conflict(_) => Row::Conflict { segment, number: starts.len() - 1, line },
            };
            rows.push(RowInfo { row, current_no: current_no + line, incoming_no: incoming_no + line });
        }
        current_no += current;
        incoming_no += incoming;
    }
    (rows, starts)
}

fn accent(c: &Colors, side: Side) -> (eframe::egui::Color32, eframe::egui::Color32) {
    match side {
        Side::Current => (c.primary, c.on_primary),
        Side::Incoming => (c.tertiary, c.on_tertiary),
    }
}

impl MergeEditor {
    pub fn open(repo: &Path, path: &str, sides: Option<(ConflictSide, ConflictSide)>) -> Result<Self, String> {
        let (original, doc) = conflict::load(repo, path)?;
        let (rows, starts) = build_rows(&doc);
        Ok(Self {
            path: path.to_string(),
            original,
            doc,
            rows,
            starts,
            sides,
            focus: 0,
            scroll_rows: true,
            scroll_result: true,
            anchor: None,
            discard_armed: None,
            split: 0.56,
        })
    }


    /// True once something was picked or edited, so closing would lose work.
    pub fn dirty(&self) -> bool {
        self.doc.resolved() > 0
    }

    pub fn set_sides(&mut self, sides: (ConflictSide, ConflictSide)) {
        self.sides = Some(sides);
    }

    fn step(&mut self, forward: bool) {
        let total = self.doc.total().max(1);
        self.focus = if forward { (self.focus + 1) % total } else { (self.focus + total - 1) % total };
        (self.scroll_rows, self.scroll_result) = (true, true);
    }

    /// Cancel: closes at once when nothing was resolved, otherwise asks once more.
    fn cancel(&mut self, now: f64) -> Action {
        let armed = self.discard_armed.is_some_and(|at| now - at < DISCARD_WINDOW);
        if !self.dirty() || armed {
            return Action::Close;
        }
        self.discard_armed = Some(now);
        Action::Stay
    }

    /// The side's checkbox: all of its lines in every conflict, some, or none.
    fn side_state(&self, side: Side) -> Check {
        let (picked, total) = self.doc.conflicts().fold((0, 0), |(picked, total), c| (picked + c.picked_count(side), total + c.lines(side).len()));
        match picked {
            0 => Check::Off,
            n if n == total => Check::On,
            _ => Check::Mixed,
        }
    }

    /// Takes or drops every line of `side`. Where that side has no lines, taking it
    /// means dropping the block.
    fn set_side(&mut self, side: Side, on: bool) {
        for conflict in self.doc.conflicts_mut() {
            match (conflict.lines(side).is_empty(), on) {
                (false, _) => conflict.set_side(side, on),
                (true, true) if !conflict.resolved => conflict.take(Take::Neither),
                (true, false) if conflict.dropped() => conflict.reset(),
                (true, _) => {}
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, rect: Rect, busy: bool) -> Action {
        let c = w::colors(ui);
        let area = Rect::from_min_max(rect.min + vec2(12.0, 0.0), rect.max - vec2(12.0, 12.0));
        let title = Rect::from_min_size(area.min, vec2(area.width(), 48.0));
        let footer = Rect::from_min_max(pos2(area.left(), area.bottom() - 52.0), area.max);
        let middle = Rect::from_min_max(pos2(area.left(), title.bottom() + 4.0), pos2(area.right(), footer.top() - 8.0));

        let top_height = (middle.height() * self.split).clamp(150.0, (middle.height() - 150.0).max(150.0));
        let top = Rect::from_min_size(middle.min, vec2(middle.width(), top_height));
        let gap = Rect::from_min_size(pos2(middle.left(), top.bottom()), vec2(middle.width(), 10.0));
        self.split += w::splitter(ui, Id::unique("merge-split"), gap, false) / middle.height().max(1.0);
        let result = Rect::from_min_max(pos2(middle.left(), gap.bottom()), middle.max);

        let mut action = Action::Stay;
        let now = ui.input(|i| i.time);
        self.title(ui, &c, title);
        self.sides_view(ui, &c, top);
        self.result_view(ui, &c, result);
        if let Some(chosen) = self.footer(ui, &c, footer, busy, now) {
            action = chosen;
        }

        // Closing the window is Cancel: it asks once more before dropping picked lines.
        let (next, previous, escape) = ui.input(|i| (i.key_pressed(Key::F7) && !i.modifiers.shift, i.key_pressed(Key::F7) && i.modifiers.shift, i.key_pressed(Key::Escape) || i.viewport().close_requested()));
        if next || previous {
            self.step(next);
        }
        if escape && matches!(action, Action::Stay) {
            action = self.cancel(now);
        }
        action
    }

    fn title(&self, ui: &Ui, c: &Colors, rect: Rect) {
        let p = ui.painter().with_clip_rect(rect);
        let mid = rect.center().y;
        w::icon_at(&p, pos2(rect.left() + 16.0, mid), ic::MERGE, 22.0, c.primary);
        let end = p.text(pos2(rect.left() + 36.0, mid), Align2::LEFT_CENTER, "Merge", theme::emph(19.0), c.on_surface).right();
        w::icon_at(&p, pos2(end + 24.0, mid), ic::FILE, 16.0, c.on_surface_variant);
        w::label_in(&p, Rect::from_min_max(pos2(end + 38.0, rect.top()), pos2(rect.right() - 16.0, rect.bottom())), &self.path, theme::body(14.0), c.on_surface_variant);
    }

    /// Both versions, side by side, scrolling together.
    fn sides_view(&mut self, ui: &mut Ui, c: &Colors, rect: Rect) {
        let half = (rect.width() - 10.0) / 2.0;
        let cards = [Rect::from_min_size(rect.min, vec2(half, rect.height())), Rect::from_min_size(pos2(rect.right() - half, rect.top()), vec2(half, rect.height()))];
        for (card, side) in cards.into_iter().zip([Side::Current, Side::Incoming]) {
            w::card(ui, card, c.surface, shape::LARGE_INCREASED);
            self.side_header(ui, c, Rect::from_min_size(card.min, vec2(card.width(), HEADER)), side);
        }

        let body = Rect::from_min_max(pos2(rect.left(), rect.top() + HEADER), rect.max - vec2(0.0, 10.0));
        let digits = self.rows.last().map_or(1, |r| r.current_no.max(r.incoming_no)).to_string().len();
        let gutter = 34.0 + digits as f32 * 7.5;
        let mut clicks = Vec::new();
        let target = self.scroll_rows.then(|| self.starts.get(self.focus).copied()).flatten();
        self.scroll_rows = false;
        let (doc, rows) = (&self.doc, &self.rows);
        w::child(ui, "merge-sides", body, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut area = ScrollArea::vertical().id_salt("merge-sides").auto_shrink(false);
            if let Some(row) = target {
                area = area.vertical_scroll_offset((row as f32 * LINE - body.height() / 3.0).max(0.0));
            }
            area.show_rows(ui, LINE, rows.len(), |ui, range| {
                for info in &rows[range] {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), LINE), Sense::hover());
                    let cells = [(Side::Current, cards[0], info.current_no), (Side::Incoming, cards[1], info.incoming_no)];
                    for (side, card, line_no) in cells {
                        let cell = Rect::from_min_max(pos2(card.left() + 6.0, row.top()), pos2(card.right() - 6.0, row.bottom()));
                        let (Row::Plain { segment, .. } | Row::Conflict { segment, .. }) = info.row;
                        match (info.row, doc.segments.get(segment)) {
                            (Row::Plain { line, .. }, Some(Segment::Plain(lines))) => plain_cell(ui.painter(), c, cell, gutter, line_no, &lines[line]),
                            (Row::Conflict { number, line, .. }, Some(Segment::Conflict(conflict))) => conflict_cell(ui, c, cell, gutter, conflict, side, number, line, line_no, &mut clicks),
                            _ => {}
                        }
                    }
                }
            });
        });
        self.apply(clicks);
    }

    fn apply(&mut self, clicks: Vec<Click>) {
        for click in clicks {
            let (number, side) = match click {
                Click::Line { number, side, .. } | Click::WholeSide { number, side } => (number, side),
            };
            let Some(conflict) = self.doc.conflicts_mut().nth(number) else { continue };
            match (click, self.anchor) {
                (Click::WholeSide { .. }, _) => conflict.set_side(side, true),
                (Click::Line { line, shift: true, .. }, Some((n, s, from))) if n == number && s == side => conflict.pick_range(side, from, line),
                (Click::Line { line, .. }, _) => {
                    conflict.toggle(side, line);
                    self.anchor = Some((number, side, line));
                }
            }
            // Follow the conflict being worked on in the result below.
            self.focus = number;
            self.scroll_result = true;
        }
    }

    /// Pane header: a checkbox for the whole side, its branch and latest commit.
    fn side_header(&mut self, ui: &mut Ui, c: &Colors, rect: Rect, side: Side) {
        let (tint, on_tint) = accent(c, side);
        let id = Id::unique(("merge-side", side == Side::Current));
        let state = self.side_state(side);
        if w::checkbox_at(ui, id, pos2(rect.left() + 24.0, rect.center().y), state, tint, on_tint).on_hover_text("Take every conflicting line from this side").clicked() {
            self.set_side(side, state != Check::On);
        }
        let info = self.sides.as_ref().map(|(current, incoming)| if side == Side::Current { current } else { incoming });
        let label = self.doc.conflicts().next().map(|k| if side == Side::Current { k.current_label.as_str() } else { k.incoming_label.as_str() });
        let name = info.map(|s| s.name.as_str()).filter(|n| !n.is_empty()).or(label).unwrap_or("");
        let role = if side == Side::Current { "Current" } else { "Incoming" };
        let reserve = if side == Side::Incoming { 150.0 } else { 12.0 };
        let p = ui.painter().with_clip_rect(rect);
        let line = Rect::from_min_max(pos2(rect.left() + 48.0, rect.top() + 9.0), pos2(rect.right() - reserve, rect.top() + 29.0));
        w::icon_at(&p, pos2(line.left() + 8.0, line.center().y), ic::BRANCH, 16.0, tint);
        let used = w::label_in(&p, line.with_min_x(line.left() + 22.0), name, theme::emph(14.0), c.on_surface);
        w::label_in(&p, line.with_min_x(line.left() + 30.0 + used), role, theme::body(12.0), c.on_surface_variant);
        let detail = match info.and_then(|s| s.tip.as_ref()) {
            Some(tip) => format!("{}  \u{00b7}  {}", tip.subject, tip.date),
            None => String::new(),
        };
        w::label_in(&p, line.translate(vec2(0.0, 19.0)).with_min_x(line.left() + 2.0), &detail, theme::body(12.0), c.on_surface_variant);
        if side == Side::Incoming {
            self.counter(ui, c, rect);
        }
    }

    /// "1/3" and the previous / next arrows, at the top right.
    fn counter(&mut self, ui: &mut Ui, c: &Colors, rect: Rect) {
        let (total, resolved) = (self.doc.total(), self.doc.resolved());
        let mid = rect.center().y;
        let (fill, fg) = if resolved == total { (c.diff_added, c.added) } else { (c.error, c.on_error) };
        let badge = Rect::from_center_size(pos2(rect.right() - 108.0, mid), vec2(46.0, 22.0));
        let p = ui.painter();
        p.rect_filled(badge, w::radius(11.0), fill);
        p.text(badge.center(), Align2::CENTER_CENTER, format!("{resolved}/{total}"), theme::emph(12.0), fg);
        ui.interact(badge, Id::unique("merge-count"), Sense::hover()).on_hover_text("Conflicts resolved");
        for (x, icon, tip, forward) in [(-62.0, ic::UP, "Previous conflict (Shift+F7)", false), (-28.0, ic::DOWN, "Next conflict (F7)", true)] {
            let button = Rect::from_center_size(pos2(rect.right() + x, mid), Vec2::splat(30.0));
            if w::icon_btn(icon, tip).size(30.0).show_at(ui, Id::unique(("merge-step", forward)), button).clicked() {
                self.step(forward);
            }
        }
    }

    /// The merged file. Text of resolved conflicts is editable; unresolved ones show
    /// as a red row with quick choices.
    fn result_view(&mut self, ui: &mut Ui, c: &Colors, rect: Rect) {
        w::card(ui, rect, c.surface, shape::LARGE_INCREASED);
        let p = ui.painter();
        p.text(pos2(rect.left() + 18.0, rect.top() + 18.0), Align2::LEFT_CENTER, "Result", theme::emph(13.5), c.on_surface);
        p.text(pos2(rect.right() - 18.0, rect.top() + 18.0), Align2::RIGHT_CENTER, "Editing allowed in conflict blocks", theme::body(12.0), c.on_surface_variant);

        let body = Rect::from_min_max(pos2(rect.left() + 6.0, rect.top() + 34.0), rect.max - vec2(6.0, 10.0));
        let (focus, total) = (self.focus, self.doc.total());
        let scroll = std::mem::take(&mut self.scroll_result);
        let digits = self.rows.len().to_string().len();
        let gutter = 18.0 + digits as f32 * 7.5;
        let doc = &mut self.doc;
        w::child(ui, "merge-result", body, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ScrollArea::vertical().id_salt("merge-result").auto_shrink(false).show(ui, |ui| {
                let row_height = ui.painter().layout_no_wrap("0".to_owned(), theme::mono(12.5), c.on_surface).size().y;
                let (mut line_no, mut number) = (1, 0);
                for segment in doc.segments.iter_mut() {
                    match segment {
                        Segment::Plain(lines) => {
                            result_plain(ui, c, lines, line_no, gutter, row_height);
                            line_no += lines.len();
                        }
                        Segment::Conflict(conflict) => {
                            let block = result_block(ui, c, conflict, number, total, line_no, gutter, row_height);
                            if scroll && number == focus {
                                ui.scroll_to_rect(block.expand2(vec2(0.0, 2.0 * row_height)), None);
                            }
                            line_no += conflict.result_lines().len();
                            number += 1;
                        }
                    }
                }
            });
        });
    }

    /// Hint on the left, Cancel and Resolve on the right.
    fn footer(&mut self, ui: &mut Ui, c: &Colors, rect: Rect, busy: bool, now: f64) -> Option<Action> {
        let (total, resolved) = (self.doc.total(), self.doc.resolved());
        let armed = self.discard_armed.is_some_and(|at| now - at < DISCARD_WINDOW);
        if armed {
            ui.ctx().request_repaint_after_secs(1.0);
        }
        let hint = "Click a line to take it  \u{00b7}  Shift+click for a range  \u{00b7}  Double-click for the whole block";
        let text = Rect::from_min_max(pos2(rect.left() + 8.0, rect.top()), pos2(rect.right() - 300.0, rect.bottom()));
        w::label_in(ui.painter(), text, hint, theme::body(12.5), c.on_surface_variant);
        let mut action = None;
        w::hchild(ui, "merge-footer", rect, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ready = resolved == total && !busy;
                let tip = if resolved == total { "Write the file and stage it (Ctrl+Enter)".to_string() } else { format!("{} of {total} conflicts still unresolved", total - resolved) };
                let save = w::btn("Resolve").icon(ic::CHECK).enabled(ready).show(ui).on_hover_text(tip);
                if ready && (save.clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter))) {
                    action = Some(Action::Save(self.doc.render()));
                }
                let (label, kind) = if armed { ("Discard changes?", Kind::Danger) } else { ("Cancel", Kind::Text) };
                if w::btn(label).kind(kind).show(ui).clicked() {
                    action = Some(self.cancel(now));
                }
            });
        });
        action
    }
}

fn code(line: &str) -> String {
    line.replace('\t', "    ")
}

fn number_at(p: &Painter, c: &Colors, cell: Rect, gutter: f32, line_no: usize) {
    p.text(pos2(cell.left() + gutter - 8.0, cell.center().y), Align2::RIGHT_CENTER, line_no, theme::mono(11.0), c.on_surface_variant.gamma_multiply(0.75));
}

/// A line both sides share.
fn plain_cell(p: &Painter, c: &Colors, cell: Rect, gutter: f32, line_no: usize, line: &str) {
    number_at(p, c, cell, gutter, line_no);
    w::label_in(&p.with_clip_rect(cell), cell.with_min_x(cell.left() + gutter), &code(line), theme::mono(12.5), c.on_surface_variant);
}

/// One side's line of a conflict: tinted, with a checkbox. A side with fewer lines
/// than the other gets a filler so the blocks stay aligned.
#[allow(clippy::too_many_arguments)]
fn conflict_cell(ui: &Ui, c: &Colors, cell: Rect, gutter: f32, conflict: &Conflict, side: Side, number: usize, line: usize, line_no: usize, clicks: &mut Vec<Click>) {
    let p = ui.painter().with_clip_rect(cell);
    let (tint, on_tint) = accent(c, side);
    let Some(text) = conflict.lines(side).get(line) else {
        p.rect_filled(cell, 0.0, c.on_surface.gamma_multiply(0.035));
        if line == 0 {
            p.text(pos2(cell.left() + gutter, cell.center().y), Align2::LEFT_CENTER, "no lines on this side", theme::body(12.0), c.on_surface_variant.gamma_multiply(0.8));
        }
        return;
    };
    let picked = conflict.is_picked(side, line);
    let id = Id::unique(("merge-line", number, side == Side::Current, line));
    let resp = ui.interact(cell, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let strength = if picked { 0.34 } else { 0.14 } + if resp.hovered() { 0.08 } else { 0.0 };
    p.rect_filled(cell, 0.0, tint.gamma_multiply(strength));
    if picked {
        p.rect_filled(Rect::from_min_size(cell.min, vec2(3.0, cell.height())), 0.0, tint);
    }
    let check = Rect::from_center_size(pos2(cell.left() + 15.0, cell.center().y), Vec2::splat(13.0));
    if picked {
        p.rect_filled(check, w::radius(3.0), tint);
        w::icon_at(&p, check.center(), ic::CHECK, 11.0, on_tint);
    } else {
        p.rect_stroke(check, w::radius(3.0), Stroke::new(1.5, c.outline), eframe::egui::StrokeKind::Inside);
    }
    number_at(&p, c, cell, gutter, line_no);
    let shown = code(text);
    let text_rect = cell.with_min_x(cell.left() + gutter).with_max_x(cell.right() - 4.0);
    w::label_in(&p, text_rect, &shown, theme::mono(12.5), c.on_surface);
    // A line cut off by the pane shows in full on hover.
    let cut = w::text_width(&p, &shown, theme::mono(12.5)) > text_rect.width();
    let resp = if cut { resp.on_hover_text(RichText::new(text.trim()).font(theme::mono(12.5))) } else { resp };
    if resp.double_clicked() {
        clicks.push(Click::WholeSide { number, side });
    } else if resp.clicked() {
        clicks.push(Click::Line { number, side, line, shift: ui.input(|i| i.modifiers.shift) });
    }
}

/// Unchanged lines of the result. Only the visible ones are painted.
fn result_plain(ui: &mut Ui, c: &Colors, lines: &[String], first_no: usize, gutter: f32, row_height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), lines.len() as f32 * row_height), Sense::hover());
    let clip = ui.clip_rect();
    let from = ((clip.top() - rect.top()) / row_height).floor().max(0.0) as usize;
    let to = (((clip.bottom() - rect.top()) / row_height).ceil().max(0.0) as usize).min(lines.len());
    let p = ui.painter();
    for (index, line) in lines.iter().enumerate().take(to).skip(from) {
        let row = Rect::from_min_size(pos2(rect.left(), rect.top() + index as f32 * row_height), vec2(rect.width(), row_height));
        number_at(p, c, row, gutter, first_no + index);
        w::label_in(p, row.with_min_x(row.left() + gutter), &code(line), theme::mono(12.5), c.on_surface);
    }
}

/// One conflict in the result: its editable text, or a red row while unresolved.
/// Returns the block's rectangle.
#[allow(clippy::too_many_arguments)]
fn result_block(ui: &mut Ui, c: &Colors, conflict: &mut Conflict, number: usize, total: usize, first_no: usize, gutter: f32, row_height: f32) -> Rect {
    let top = ui.cursor().top();
    let width = ui.available_width();
    if !conflict.resolved {
        let (strip, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(strip.shrink2(vec2(0.0, 1.0)), w::radius(6.0), c.error_container);
        w::icon_at(p, pos2(strip.left() + gutter - 12.0, strip.center().y), ic::WARNING, 15.0, c.on_error_container);
        let label = format!("Merge conflict {} of {total}: pick lines above, or choose", number + 1);
        let mut x = p.text(pos2(strip.left() + gutter, strip.center().y), Align2::LEFT_CENTER, label, theme::emph(12.5), c.on_error_container).right() + 10.0;
        // Quick choices, as links in the strip.
        for (name, take) in [("Current", Take::Current), ("Incoming", Take::Incoming), ("Both", Take::Both), ("Neither", Take::Neither)] {
            let size = vec2(w::text_width(p, name, theme::emph(12.5)) + 16.0, 20.0);
            let link = Rect::from_min_size(pos2(x, strip.center().y - 10.0), size);
            let resp = ui.interact(link, Id::unique(("merge-take", number, name)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            let fill = c.on_error_container.gamma_multiply(if resp.hovered() { 0.22 } else { 0.10 });
            p.rect_filled(link, w::radius(10.0), fill);
            p.text(link.center(), Align2::CENTER_CENTER, name, theme::emph(12.5), c.on_error_container);
            if resp.clicked() {
                conflict.take(take);
            }
            x += size.x + 6.0;
        }
    }

    // The model ends every line with '\n'. The box shows the text without the last
    // one, so there is no empty row at the bottom; a row added there is a real blank line.
    let shown = conflict.result.strip_suffix('\n').unwrap_or(&conflict.result).to_string();
    let mut text = shown.clone();
    let lines = conflict.result_lines().len();
    let fill = if conflict.resolved { c.added.gamma_multiply(0.14) } else { c.surface_highest };
    let field = Frame::new().fill(fill).inner_margin(Margin { left: gutter.round() as i8, right: 8, top: 0, bottom: 0 });
    let mut edit = TextEdit::multiline(&mut text).id_salt(("merge-result", number)).code_editor().font(theme::mono(12.5)).desired_width(f32::INFINITY).desired_rows(lines.max(1)).frame(field);
    if !conflict.resolved {
        edit = edit.hint_text("or type the resolution here");
    }
    let field_rect = ui.add(edit).rect;
    if text != shown {
        conflict.edit(&if text.is_empty() { text } else { text + "\n" });
    }

    let block = Rect::from_min_max(pos2(field_rect.left(), top), field_rect.max);
    if conflict.resolved {
        let p = ui.painter();
        p.rect_filled(Rect::from_min_size(field_rect.min, vec2(3.0, field_rect.height())), 0.0, c.added);
        // Number only the rows in view: a both-added file can be thousands of lines.
        let clip = ui.clip_rect();
        let from = ((clip.top() - field_rect.top()) / row_height).floor().max(0.0) as usize;
        let to = (((clip.bottom() - field_rect.top()) / row_height).ceil().max(0.0) as usize).min(lines);
        for index in from..to {
            let row = Rect::from_min_size(pos2(field_rect.left(), field_rect.top() + index as f32 * row_height), vec2(width, row_height));
            number_at(p, c, row, gutter, first_no + index);
        }
        // Undo for this block, shown while the pointer is over it.
        if ui.rect_contains_pointer(block) {
            let button = Rect::from_center_size(pos2(block.right() - 16.0, block.top() + 12.0), Vec2::splat(22.0));
            if w::icon_btn(ic::RESET, "Start this conflict over").size(22.0).show_at(ui, Id::unique(("merge-reset", number)), button).clicked() {
                conflict.reset();
            }
        }
    }
    block
}
