//! One open repository: state, background work, and its three pages
//! (history, local changes, stashes).

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::epaint::{CubicBezierShape, QuadraticBezierShape};
use eframe::egui::{self, Align, Align2, Color32, Context, Id, Key, Layout, Painter, Pos2, Rect, Response, ScrollArea, Sense, Stroke, Ui, pos2, vec2};

use crate::dialogs::{self, Dialog, Outcome};
use crate::diff::{self, Diff, LineKind};
use crate::git::{self, Branch, Change, Commit, DiffSpec, InProgress, LogQuery, Op, OpResult, RefKind, SearchBy, Snapshot, Stash, short};
use crate::graph::{self, DotKind, Graph};
use crate::theme::{self, Colors, shape};
use crate::widgets::{self as w, Kind, ic};

const GAP: f32 = 10.0;
const CARD: f32 = shape::LARGE_INCREASED;
const RAIL: f32 = 80.0;
const ROW: f32 = 30.0;

#[derive(Clone, Copy, PartialEq)]
enum Page {
    History,
    Changes,
    Stashes,
}

/// What the detail cards show: a commit or a stash.
struct Detail {
    sha: String,
    base: String,
    message: String,
    files: Vec<Change>,
    commit: Option<Commit>,
    stash: Option<Stash>,
}

enum Msg {
    Snapshot(Box<Snapshot>, Graph),
    Done(&'static str, OpResult),
    Detail(Detail),
    Diff(u64, Diff),
    LastMessage(String),
    Text(String, String),
}

struct Notice {
    text: String,
    error: bool,
    at: f64,
}

pub struct RepoView {
    pub path: PathBuf,
    pub name: String,
    /// A repository (submodule, worktree) the user asked to open in a new tab.
    pub open_request: Option<PathBuf>,
    snap: Option<Arc<Snapshot>>,
    graph: Arc<Graph>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    loading: bool,
    busy: Option<&'static str>,
    log: String,
    show_log: bool,
    text_view: Option<(String, String)>,
    notice: Option<Notice>,
    dialog: Option<Dialog>,
    page: Page,
    query: LogQuery,
    search: String,
    search_by: SearchBy,
    filter: String,
    open: [bool; 5],
    selected: Option<String>,
    scroll_to: Option<usize>,
    wanted: Option<String>,
    detail: Option<Arc<Detail>>,
    detail_file: Option<usize>,
    picked: Option<(String, bool)>,
    message: String,
    amend: bool,
    signoff: bool,
    stash: Option<String>,
    diff_generation: u64,
    diff_spec: Option<DiffSpec>,
    diff: Option<Arc<Diff>>,
    diff_title: String,
    sidebar_w: f32,
    files_w: f32,
    split: f32,
}

/// `2026-01-31 12:00:59 +0100` -> `2026-01-31 12:00`, matching commit dates.
fn minutes(date: &str) -> &str {
    date.get(..16).unwrap_or(date)
}

fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').map_or(("", path), |(dir, name)| (dir, name))
}

impl RepoView {
    pub fn new(path: PathBuf, history_limit: usize, ctx: &Context) -> Self {
        let (tx, rx) = channel();
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let mut view = Self {
            path,
            name,
            open_request: None,
            snap: None,
            graph: Arc::new(Graph::default()),
            tx,
            rx,
            loading: false,
            busy: None,
            log: String::new(),
            show_log: false,
            text_view: None,
            notice: None,
            dialog: None,
            page: Page::History,
            query: LogQuery { limit: history_limit, ..Default::default() },
            search: String::new(),
            search_by: SearchBy::Message,
            filter: String::new(),
            open: [true, true, false, true, true],
            selected: None,
            scroll_to: None,
            wanted: None,
            detail: None,
            detail_file: None,
            picked: None,
            message: String::new(),
            amend: false,
            signoff: false,
            stash: None,
            diff_generation: 0,
            diff_spec: None,
            diff: None,
            diff_title: String::new(),
            sidebar_w: 280.0,
            files_w: 360.0,
            split: 0.52,
        };
        view.reload(ctx);
        view
    }

    fn id(&self, what: &str) -> Id {
        Id::unique((&self.path, what))
    }

    // ------------------------------------------------------------ background work

    fn spawn(&self, ctx: &Context, work: impl FnOnce(&Path) -> Msg + Send + 'static) {
        let (tx, ctx, path) = (self.tx.clone(), ctx.clone(), self.path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(work(&path)); // receiver gone = tab closed
            ctx.request_repaint();
        });
    }

    pub fn reload(&mut self, ctx: &Context) {
        if self.loading {
            return;
        }
        self.loading = true;
        let query = self.query.clone();
        self.spawn(ctx, move |repo| {
            let snap = git::snapshot(repo, &query);
            // Search results are not contiguous history, so they get no graph.
            let graph = if query.search.is_some() { Graph::default() } else { graph::layout(&snap.commits) };
            Msg::Snapshot(Box::new(snap), graph)
        });
    }

    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    fn run(&mut self, ctx: &Context, ops: Vec<Op>) {
        let Some(first) = ops.first() else { return };
        if let Some(running) = self.busy {
            self.notify(format!("{running} is still running"), true, ctx);
            return;
        }
        let title = first.title();
        self.busy = Some(title);
        self.spawn(ctx, move |repo| {
            let mut total = OpResult { ok: true, ..Default::default() };
            for op in &ops {
                let result = git::run_op(repo, op);
                total.log += &result.log;
                if !result.ok {
                    total.ok = false;
                    total.error = result.error;
                    break;
                }
            }
            Msg::Done(title, total)
        });
    }

    fn notify(&mut self, text: String, error: bool, ctx: &Context) {
        self.notice = Some(Notice { text, error, at: ctx.input(|i| i.time) });
    }

    fn ask(&mut self, dialog: Result<Dialog, String>, ctx: &Context) {
        match dialog {
            Ok(dialog) => self.dialog = Some(dialog),
            Err(reason) => self.notify(reason, true, ctx),
        }
    }

    pub fn poll(&mut self, ctx: &Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Snapshot(snap, graph) => self.on_snapshot(ctx, *snap, graph),
                Msg::Done(title, result) => self.on_done(ctx, title, result),
                Msg::Detail(detail) if self.wanted.as_deref() == Some(&detail.sha) => {
                    let first = detail.files.first().cloned();
                    self.detail = Some(Arc::new(detail));
                    self.detail_file = None;
                    if let Some(change) = first {
                        self.pick_file(ctx, 0, change);
                    }
                }
                Msg::Diff(generation, diff) if generation == self.diff_generation => self.diff = Some(Arc::new(diff)),
                Msg::LastMessage(message) if self.message.trim().is_empty() => self.message = message,
                Msg::Text(title, text) => self.text_view = Some((title, text)),
                Msg::Detail(_) | Msg::Diff(..) | Msg::LastMessage(_) => {} // stale
            }
        }
    }

    fn on_done(&mut self, ctx: &Context, title: &'static str, result: OpResult) {
        self.busy = None;
        self.log += &result.log;
        if !result.ok {
            self.notify(format!("{title} failed: {}", result.error.trim()), true, ctx);
        } else if matches!(title, "Fetch" | "Pull" | "Push" | "Commit") {
            self.notify(format!("{title} finished"), false, ctx);
        }
        if result.ok && title == "Commit" {
            self.message.clear();
            self.amend = false;
        }
        self.loading = false; // a snapshot from before the operation is stale; ask again
        self.reload(ctx);
    }

    fn on_snapshot(&mut self, ctx: &Context, snap: Snapshot, graph: Graph) {
        self.loading = false;
        if let Some((path, staged)) = self.picked.clone() {
            let still = snap.changes.iter().find(|c| c.path == path && if staged { c.staged() } else { c.unstaged() }).cloned();
            match still {
                Some(change) if self.page == Page::Changes => self.pick_change(ctx, &change, staged),
                Some(_) => {}
                None => {
                    self.picked = None;
                    if self.page == Page::Changes {
                        self.clear_diff();
                    }
                }
            }
        }
        if self.stash.as_ref().is_some_and(|sha| !snap.stashes.iter().any(|s| &s.sha == sha)) {
            self.stash = None;
            if self.page == Page::Stashes {
                self.clear_detail();
            }
        }
        self.snap = Some(Arc::new(snap));
        self.graph = Arc::new(graph);
    }

    // ------------------------------------------------------------ selection

    fn clear_diff(&mut self) {
        self.diff_generation += 1;
        self.diff_spec = None;
        self.diff = None;
        self.diff_title.clear();
    }

    fn clear_detail(&mut self) {
        self.wanted = None;
        self.detail = None;
        self.detail_file = None;
        self.clear_diff();
    }

    fn show_diff(&mut self, ctx: &Context, spec: DiffSpec, title: String) {
        self.diff_generation += 1;
        let generation = self.diff_generation;
        if self.diff_spec.as_ref() != Some(&spec) {
            self.diff = None; // new target: show loading; same target: keep the old diff until refreshed
        }
        self.diff_spec = Some(spec.clone());
        self.diff_title = title;
        self.spawn(ctx, move |repo| Msg::Diff(generation, diff::parse(&git::diff_text(repo, &spec))));
    }

    fn load_detail(&mut self, ctx: &Context, sha: String, base: String, commit: Option<Commit>, stash: Option<Stash>) {
        self.clear_detail();
        self.wanted = Some(sha.clone());
        self.spawn(ctx, move |repo| {
            Msg::Detail(Detail { message: git::full_message(repo, &sha), files: git::changed_files(repo, &base, &sha), sha, base, commit, stash })
        });
    }

    fn select_commit(&mut self, ctx: &Context, commit: &Commit) {
        self.selected = Some(commit.sha.clone());
        self.load_detail(ctx, commit.sha.clone(), git::base_of(commit), Some(commit.clone()), None);
    }

    fn select_stash(&mut self, ctx: &Context, stash: &Stash) {
        self.stash = Some(stash.sha.clone());
        self.load_detail(ctx, stash.sha.clone(), format!("{}^", stash.sha), None, Some(stash.clone()));
    }

    /// Selects file `index` of the current detail.
    fn pick_file(&mut self, ctx: &Context, index: usize, change: Change) {
        let Some(detail) = self.detail.clone() else { return };
        self.detail_file = Some(index);
        let title = change.path.clone();
        self.show_diff(ctx, DiffSpec::Revs { base: detail.base.clone(), rev: detail.sha.clone(), change }, title);
    }

    fn pick_change(&mut self, ctx: &Context, change: &Change, staged: bool) {
        self.picked = Some((change.path.clone(), staged));
        let spec = if staged { DiffSpec::Staged(change.clone()) } else { DiffSpec::Unstaged(change.clone()) };
        self.show_diff(ctx, spec, change.path.clone());
    }

    fn enter(&mut self, ctx: &Context, page: Page) {
        if self.page == page {
            return;
        }
        self.page = page;
        self.clear_detail();
        let Some(snap) = self.snap.clone() else { return };
        match page {
            Page::History => {
                if let Some(commit) = snap.commits.iter().find(|c| Some(&c.sha) == self.selected.as_ref()) {
                    self.select_commit(ctx, commit);
                }
            }
            Page::Changes => {
                let picked = self.picked.clone().and_then(|(path, staged)| Some((snap.changes.iter().find(|c| c.path == path)?.clone(), staged)));
                if let Some((change, staged)) = picked {
                    self.pick_change(ctx, &change, staged);
                }
            }
            Page::Stashes => {
                if let Some(stash) = snap.stashes.iter().find(|s| Some(&s.sha) == self.stash.as_ref()) {
                    self.select_stash(ctx, stash);
                }
            }
        }
    }

    /// Shows `sha` in the history page, if it is loaded.
    fn jump_to(&mut self, ctx: &Context, snap: &Snapshot, sha: &str) {
        let Some(index) = snap.commits.iter().position(|c| c.sha == sha) else {
            self.notify(format!("{} is not in the loaded history", short(sha)), false, ctx);
            return;
        };
        self.enter(ctx, Page::History);
        self.scroll_to = Some(index);
        self.select_commit(ctx, &snap.commits[index]);
    }

    fn apply_search(&mut self, ctx: &Context) {
        let text = self.search.trim();
        self.query.search = (!text.is_empty()).then(|| (self.search_by, text.to_string()));
        self.loading = false;
        self.reload(ctx);
    }

    fn file_history(&mut self, ctx: &Context, path: &str) {
        self.search = path.to_string();
        self.search_by = SearchBy::Path;
        self.enter(ctx, Page::History);
        self.apply_search(ctx);
    }

    fn blame(&mut self, ctx: &Context, rev: String, path: String) {
        self.spawn(ctx, move |repo| Msg::Text(format!("Blame: {path}"), git::blame(repo, &rev, &path)));
    }

    // ------------------------------------------------------------ layout

    pub fn ui(&mut self, ui: &mut Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        if ui.input(|i| i.key_pressed(Key::F5)) {
            self.reload(&ctx);
        }
        let Some(snap) = self.snap.clone() else {
            w::loading_indicator(ui, rect.center(), 26.0);
            return;
        };
        let rail = Rect::from_min_size(rect.min, vec2(RAIL, rect.height()));
        let content = Rect::from_min_max(pos2(rail.right(), rect.top()), rect.max - vec2(GAP + 2.0, GAP + 2.0));
        self.rail(ui, rail, &snap);

        self.sidebar_w = self.sidebar_w.clamp(200.0, (content.width() - 520.0).max(200.0));
        let sidebar = Rect::from_min_size(content.min, vec2(self.sidebar_w, content.height()));
        let gap = Rect::from_min_size(pos2(sidebar.right(), content.top()), vec2(GAP, content.height()));
        self.sidebar_w += w::splitter(ui, self.id("sidebar-split"), gap, true);
        self.sidebar(ui, sidebar, &snap);

        let main = Rect::from_min_max(pos2(gap.right(), content.top()), content.max);
        let toolbar = Rect::from_min_size(main.min, vec2(main.width(), 52.0));
        self.toolbar(ui, toolbar, &snap);
        if self.busy.is_some() || self.loading {
            w::wavy_progress(ui, Rect::from_min_size(pos2(main.left() + 26.0, toolbar.bottom() + 1.0), vec2(main.width() - 52.0, GAP - 2.0)));
        }
        let page = Rect::from_min_max(pos2(main.left(), toolbar.bottom() + GAP), main.max);
        match self.page {
            Page::History => self.history_page(ui, page, &snap),
            Page::Changes => self.changes_page(ui, page, &snap),
            Page::Stashes => self.stashes_page(ui, page, &snap),
        }
        self.overlays(&ctx);
    }

    fn rail(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let ctx = ui.ctx().clone();
        w::child(ui, "rail", rect, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 6.0);
            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                let items = [
                    (Page::History, ic::HISTORY, "History", 0),
                    (Page::Changes, ic::CHANGES, "Changes", snap.changes.len()),
                    (Page::Stashes, ic::STASH, "Stashes", snap.stashes.len()),
                ];
                for (page, icon, label, badge) in items {
                    if w::nav_item(ui, icon, label, self.page == page, badge).clicked() {
                        self.enter(&ctx, page);
                    }
                }
            });
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                ui.add_space(10.0);
                if w::icon_btn(ic::TERMINAL, "Command log").selected(self.show_log).show(ui).clicked() {
                    self.show_log = !self.show_log;
                }
                if w::icon_btn(ic::REFRESH, "Refresh (F5)").show(ui).clicked() {
                    self.reload(&ctx);
                }
            });
        });
    }

    // ------------------------------------------------------------ toolbar

    fn toolbar(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        w::card(ui, rect, c.surface, rect.height() / 2.0);
        let current = snap.branches.iter().find(|b| b.current);
        let (ahead, behind) = current.map_or((0, 0), |b| (b.ahead, b.behind));
        let idle = self.busy.is_none();
        w::hchild(ui, "toolbar", rect.shrink2(vec2(8.0, 0.0)), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if w::btn("Fetch").icon(ic::FETCH).kind(Kind::Text).enabled(idle).show(ui).clicked() {
                self.run(&ctx, vec![Op::Fetch { remote: None, prune: true }]);
            }
            let pull_label = if behind > 0 { format!("Pull {behind}") } else { "Pull".into() };
            let pull_kind = if behind > 0 { Kind::Tonal } else { Kind::Text };
            if w::btn(&pull_label).icon(ic::PULL).kind(pull_kind).enabled(idle).show(ui).clicked() {
                self.ask(dialogs::pull(snap), &ctx);
            }
            let push_label = if ahead > 0 { format!("Push {ahead}") } else { "Push".into() };
            let push_kind = if ahead > 0 { Kind::Filled } else { Kind::Text };
            if w::btn(&push_label).icon(ic::PUSH).kind(push_kind).enabled(idle).show(ui).clicked() {
                self.ask(dialogs::push(snap), &ctx);
            }
            let (divider, _) = ui.allocate_exact_size(vec2(13.0, 24.0), Sense::hover());
            ui.painter().vline(divider.center().x, divider.y_range(), Stroke::new(1.0, c.outline_variant));
            if w::icon_btn(ic::STASH, "Stash changes").enabled(idle).show(ui).clicked() {
                self.dialog = Some(dialogs::stash());
            }
            if w::icon_btn(ic::BRANCH, "New branch").enabled(idle).show(ui).clicked() {
                self.dialog = Some(dialogs::create_branch("HEAD".into(), "the current commit"));
            }
            if w::icon_btn(ic::TAG, "New tag").enabled(idle).show(ui).clicked() {
                self.dialog = Some(dialogs::create_tag("HEAD".into(), "the current commit"));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if w::icon_btn(ic::FOLDER_OPEN, "Open in file manager").show(ui).clicked() {
                    ctx.open_url(egui::OpenUrl::new_tab(format!("file://{}", self.path.display())));
                }
                if let Some(title) = self.busy {
                    let (spot, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                    w::loading_indicator(ui, spot.center(), 11.0);
                    ui.label(egui::RichText::new(format!("{title}\u{2026}")).font(theme::emph(13.0)).color(c.on_surface_variant));
                }
            });
        });
    }

    // ------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        w::card(ui, rect, c.surface, CARD);
        w::child(ui, "sidebar", rect.shrink(10.0), |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), 68.0), Sense::hover());
            let p = ui.painter().clone();
            let title = Rect::from_min_size(head.min + vec2(8.0, 4.0), vec2(head.width() - 16.0, 26.0));
            w::label_in(&p, title, &self.name, theme::emph(19.0), c.on_surface);
            let mut x = head.left() + 6.0;
            let mid = head.top() + 48.0;
            x += w::chip(&p, pos2(x, mid), ic::BRANCH, &snap.head.label(), c.primary_container, c.on_primary_container) + 6.0;
            if snap.state != InProgress::None {
                w::chip(&p, pos2(x, mid), ic::WARNING, snap.state.label(), c.tertiary_container, c.on_tertiary_container);
            }
            ui.add_space(6.0);
            let width = ui.available_width();
            w::text_field(ui, &mut self.filter, "Filter branches and tags", Some(ic::FILTER), width);
            ui.add_space(8.0);
            ScrollArea::vertical().id_salt("sidebar-scroll").auto_shrink(false).show(ui, |ui| self.sidebar_sections(ui, &ctx, snap, &c));
        });
    }

    fn sidebar_sections(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot, c: &Colors) {
        let needle = self.filter.to_lowercase();
        let shown = |name: &str| needle.is_empty() || name.to_lowercase().contains(&needle);

        let locals: Vec<&Branch> = snap.branches.iter().filter(|b| b.remote.is_none() && shown(&b.name)).collect();
        if w::section_header(ui, "Branches", locals.len(), Some(&mut self.open[0]), Some((ic::ADD, "New branch"))) {
            self.dialog = Some(dialogs::create_branch("HEAD".into(), "the current commit"));
        }
        for branch in locals.iter().take(visible(self.open[0])) {
            let track = match (branch.gone, branch.ahead, branch.behind) {
                (true, ..) => "gone".to_string(),
                (_, 0, 0) => String::new(),
                (_, a, 0) => format!("\u{2191}{a}"),
                (_, 0, b) => format!("\u{2193}{b}"),
                (_, a, b) => format!("\u{2191}{a} \u{2193}{b}"),
            };
            let (icon, tint) = if branch.current { (ic::CHECK_CIRCLE, c.primary) } else { (ic::BRANCH, c.on_surface_variant) };
            let resp = side_row(ui, c, 0, icon, tint, &branch.name, branch.current, &track);
            if resp.double_clicked() && !branch.current {
                self.run(ctx, vec![Op::Checkout { target: branch.name.clone() }]);
            } else if resp.clicked() {
                self.jump_to(ctx, snap, &branch.sha);
            }
            resp.context_menu(|ui| self.local_branch_menu(ui, ctx, snap, branch));
        }

        if w::section_header(ui, "Remotes", snap.remotes.len(), Some(&mut self.open[1]), Some((ic::ADD, "Add remote"))) {
            self.dialog = Some(dialogs::add_remote(snap.remotes.is_empty()));
        }
        for remote in snap.remotes.iter().take(visible(self.open[1])) {
            let resp = side_row(ui, c, 0, ic::CLOUD, c.on_surface_variant, &remote.name, true, "");
            resp.context_menu(|ui| {
                if w::menu_item(ui, ic::FETCH, "Fetch") {
                    self.run(ctx, vec![Op::Fetch { remote: Some(remote.name.clone()), prune: true }]);
                }
                if w::menu_item(ui, ic::COPY, "Copy URL") {
                    ctx.copy_text(remote.url.clone());
                }
                w::menu_separator(ui);
                if w::menu_danger(ui, ic::DELETE, "Remove remote\u{2026}") {
                    self.dialog = Some(dialogs::remove_remote(remote.name.clone()));
                }
            });
            let prefix = format!("{}/", remote.name);
            for branch in snap.branches.iter().filter(|b| b.remote.as_deref() == Some(&remote.name) && shown(&b.name)) {
                let label = branch.name.strip_prefix(&prefix).unwrap_or(&branch.name);
                let resp = side_row(ui, c, 1, ic::BRANCH, c.on_surface_variant, label, false, "");
                if resp.double_clicked() {
                    self.dialog = Some(dialogs::checkout_remote(branch.name.clone(), &remote.name));
                } else if resp.clicked() {
                    self.jump_to(ctx, snap, &branch.sha);
                }
                resp.context_menu(|ui| self.remote_branch_menu(ui, ctx, snap, branch, &remote.name));
            }
        }

        // ponytail: plain (non-virtualised) rows; cap the tag list, raise or virtualise if repos need more.
        let tags: Vec<&git::Tag> = snap.tags.iter().filter(|t| shown(&t.name)).take(300).collect();
        w::section_header(ui, "Tags", snap.tags.len(), Some(&mut self.open[2]), None);
        for tag in tags.iter().take(visible(self.open[2])) {
            let resp = side_row(ui, c, 0, ic::TAG, c.on_surface_variant, &tag.name, false, "");
            if resp.clicked() {
                self.jump_to(ctx, snap, &tag.sha);
            }
            resp.context_menu(|ui| self.tag_menu(ui, ctx, snap, tag));
        }

        if !snap.submodules.is_empty() {
            if w::section_header(ui, "Submodules", snap.submodules.len(), Some(&mut self.open[3]), Some((ic::REFRESH, "Update submodules"))) {
                self.run(ctx, vec![Op::SubmoduleUpdate]);
            }
            for module in snap.submodules.iter().take(visible(self.open[3])) {
                let resp = side_row(ui, c, 0, ic::SUBMODULE, c.on_surface_variant, module, false, "");
                if resp.double_clicked() {
                    self.open_request = Some(self.path.join(module));
                }
                resp.on_hover_text("Double-click to open in a new tab");
            }
        }

        if snap.worktrees.len() > 1 {
            w::section_header(ui, "Worktrees", snap.worktrees.len(), Some(&mut self.open[4]), None);
            for tree in snap.worktrees.iter().take(visible(self.open[4])) {
                let (_, name) = split_path(&tree.path);
                let resp = side_row(ui, c, 0, ic::WORKTREE, c.on_surface_variant, name, false, &tree.branch);
                if resp.double_clicked() {
                    self.open_request = Some(PathBuf::from(&tree.path));
                }
                resp.on_hover_text(&tree.path);
            }
        }
    }

    fn local_branch_menu(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot, branch: &Branch) {
        let name = branch.name.clone();
        if !branch.current {
            if w::menu_item(ui, ic::CHECK, "Check out") {
                self.run(ctx, vec![Op::Checkout { target: name.clone() }]);
            }
            if w::menu_item(ui, ic::MERGE, "Merge into current branch\u{2026}") {
                self.ask(dialogs::merge(snap, name.clone()), ctx);
            }
            if w::menu_item(ui, ic::REBASE, "Rebase current branch onto this\u{2026}") {
                self.ask(dialogs::rebase(snap, name.clone()), ctx);
            }
        } else {
            if w::menu_item(ui, ic::PULL, "Pull\u{2026}") {
                self.ask(dialogs::pull(snap), ctx);
            }
            if w::menu_item(ui, ic::PUSH, "Push\u{2026}") {
                self.ask(dialogs::push(snap), ctx);
            }
        }
        w::menu_separator(ui);
        if w::menu_item(ui, ic::BRANCH, "New branch from here\u{2026}") {
            self.dialog = Some(dialogs::create_branch(name.clone(), &name));
        }
        if w::menu_item(ui, ic::EDIT, "Rename\u{2026}") {
            self.dialog = Some(dialogs::rename_branch(name.clone()));
        }
        if w::menu_item(ui, ic::COPY, "Copy name") {
            ctx.copy_text(name.clone());
        }
        if !branch.current {
            w::menu_separator(ui);
            if w::menu_danger(ui, ic::DELETE, "Delete\u{2026}") {
                self.dialog = Some(dialogs::delete_branch(branch));
            }
        }
    }

    fn remote_branch_menu(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot, branch: &Branch, remote: &str) {
        let name = branch.name.clone();
        if w::menu_item(ui, ic::CHECK, "Check out as local branch\u{2026}") {
            self.dialog = Some(dialogs::checkout_remote(name.clone(), remote));
        }
        if w::menu_item(ui, ic::MERGE, "Merge into current branch\u{2026}") {
            self.ask(dialogs::merge(snap, name.clone()), ctx);
        }
        if w::menu_item(ui, ic::REBASE, "Rebase current branch onto this\u{2026}") {
            self.ask(dialogs::rebase(snap, name.clone()), ctx);
        }
        w::menu_separator(ui);
        if w::menu_item(ui, ic::BRANCH, "New branch from here\u{2026}") {
            self.dialog = Some(dialogs::create_branch(name.clone(), &name));
        }
        if w::menu_item(ui, ic::COPY, "Copy name") {
            ctx.copy_text(name);
        }
        w::menu_separator(ui);
        if w::menu_danger(ui, ic::DELETE, "Delete on remote\u{2026}") {
            self.dialog = Some(dialogs::delete_branch(branch));
        }
    }

    fn tag_menu(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot, tag: &git::Tag) {
        let name = tag.name.clone();
        if w::menu_item(ui, ic::BRANCH, "New branch from tag\u{2026}") {
            self.dialog = Some(dialogs::create_branch(name.clone(), &name));
        }
        if w::menu_item(ui, ic::COMMIT, "Check out\u{2026}") {
            self.dialog = Some(dialogs::checkout_detached(name.clone(), &name));
        }
        if w::menu_item(ui, ic::PUSH, "Push to remote\u{2026}") {
            self.ask(dialogs::push_tag(snap, name.clone(), false), ctx);
        }
        if w::menu_item(ui, ic::COPY, "Copy name") {
            ctx.copy_text(name.clone());
        }
        w::menu_separator(ui);
        if w::menu_danger(ui, ic::CLOUD, "Delete on remote\u{2026}") {
            self.ask(dialogs::push_tag(snap, name.clone(), true), ctx);
        }
        if w::menu_danger(ui, ic::DELETE, "Delete\u{2026}") {
            self.dialog = Some(dialogs::delete_tag(name));
        }
    }

    // ------------------------------------------------------------ history

    /// Top card with a list, bottom row with the file list and diff cards.
    fn list_and_detail(&mut self, ui: &mut Ui, rect: Rect) -> (Rect, Rect) {
        let list_h = (rect.height() * self.split).clamp(120.0, (rect.height() - 200.0).max(120.0));
        let list = Rect::from_min_size(rect.min, vec2(rect.width(), list_h));
        let gap = Rect::from_min_size(pos2(rect.left(), list.bottom()), vec2(rect.width(), GAP));
        self.split += w::splitter(ui, self.id("list-split"), gap, false) / rect.height().max(1.0);
        (list, Rect::from_min_max(pos2(rect.left(), gap.bottom()), rect.max))
    }

    fn history_page(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let (list, detail) = self.list_and_detail(ui, rect);
        self.history_list(ui, list, snap);
        self.detail_cards(ui, detail, (ic::COMMIT, "No commit selected", "Pick a commit to see what changed"));
    }

    fn history_list(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        w::card(ui, rect, c.surface, CARD);
        let inner = rect.shrink(8.0);
        let header = Rect::from_min_size(inner.min, vec2(inner.width(), 44.0));
        w::hchild(ui, "history-header", header, |ui| {
            let resp = w::text_field(ui, &mut self.search, "Search commits", Some(ic::SEARCH), (header.width() - 520.0).clamp(160.0, 360.0));
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                self.apply_search(&ctx);
            }
            let by = [
                (SearchBy::Message, None, "Message"),
                (SearchBy::Author, None, "Author"),
                (SearchBy::Path, None, "File"),
                (SearchBy::Content, None, "Content"),
            ];
            if w::segmented(ui, "search-by", &mut self.search_by, &by, 32.0) && !self.search.trim().is_empty() {
                self.apply_search(&ctx);
            }
            if self.query.search.is_some() && w::icon_btn(ic::CLOSE, "Clear search").size(32.0).show(ui).clicked() {
                self.search.clear();
                self.apply_search(&ctx);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if w::switch(ui, &mut self.query.all_branches, "All branches").changed() {
                    self.loading = false;
                    self.reload(&ctx);
                }
            });
        });

        let body = Rect::from_min_max(pos2(inner.left(), header.bottom() + 6.0), inner.max);
        if snap.commits.is_empty() {
            let (title, hint) = if self.query.search.is_some() { ("No matching commits", "Try another search") } else { ("No commits yet", "Make the first commit from Changes") };
            w::empty_state(ui, body, ic::HISTORY, title, hint);
            return;
        }
        let graph = self.graph.clone();
        let scroll_to = self.scroll_to.take();
        w::rows(ui, "history-rows", body, ROW, snap.commits.len(), scroll_to, |ui, range| {
            let origin = pos2(ui.max_rect().left() + 4.0, ui.max_rect().top() - range.start as f32 * ROW);
            for i in range.clone() {
                let commit = &snap.commits[i];
                let (row, resp) = w::row(ui, ROW);
                let selected = self.selected.as_deref() == Some(commit.sha.as_str());
                w::row_bg(ui, row, &resp, selected);
                if resp.clicked() {
                    self.select_commit(&ctx, commit);
                }
                resp.context_menu(|ui| self.commit_menu(ui, &ctx, snap, commit));
                let margin = graph.margins.get(i).copied().unwrap_or(10.0);
                paint_commit_row(ui.painter(), &c, row, commit, margin, selected);
            }
            paint_graph(ui.painter(), &graph, &c, origin, &range);
        });
    }

    fn commit_menu(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot, commit: &Commit) {
        let label = format!("{} \u{201c}{}\u{201d}", short(&commit.sha), commit.subject);
        if w::menu_item(ui, ic::COMMIT, "Check out this commit\u{2026}") {
            self.dialog = Some(dialogs::checkout_detached(commit.sha.clone(), &label));
        }
        if w::menu_item(ui, ic::BRANCH, "New branch here\u{2026}") {
            self.dialog = Some(dialogs::create_branch(commit.sha.clone(), &label));
        }
        if w::menu_item(ui, ic::TAG, "New tag here\u{2026}") {
            self.dialog = Some(dialogs::create_tag(commit.sha.clone(), &label));
        }
        w::menu_separator(ui);
        if w::menu_item(ui, ic::PICK, "Cherry-pick\u{2026}") {
            self.ask(dialogs::cherry_pick(snap, commit), ctx);
        }
        if w::menu_item(ui, ic::UNDO, "Revert\u{2026}") {
            self.dialog = Some(dialogs::revert(commit));
        }
        if w::menu_danger(ui, ic::RESET, "Reset current branch to here\u{2026}") {
            self.ask(dialogs::reset(snap, commit), ctx);
        }
        w::menu_separator(ui);
        if w::menu_item(ui, ic::COPY, "Copy SHA") {
            ctx.copy_text(commit.sha.clone());
        }
        if w::menu_item(ui, ic::COPY, "Copy subject") {
            ctx.copy_text(commit.subject.clone());
        }
    }

    // ------------------------------------------------------------ detail cards

    fn split_files(&mut self, ui: &Ui, salt: &str, rect: Rect) -> (Rect, Rect) {
        self.files_w = self.files_w.clamp(220.0, (rect.width() - 280.0).max(220.0));
        let left = Rect::from_min_size(rect.min, vec2(self.files_w, rect.height()));
        let gap = Rect::from_min_size(pos2(left.right(), rect.top()), vec2(GAP, rect.height()));
        self.files_w += w::splitter(ui, self.id(salt), gap, true);
        (left, Rect::from_min_max(pos2(gap.right(), rect.top()), rect.max))
    }

    fn detail_cards(&mut self, ui: &mut Ui, rect: Rect, empty: (&str, &str, &str)) {
        let c = w::colors(ui);
        let Some(detail) = self.detail.clone() else {
            w::card(ui, rect, c.surface, CARD);
            if self.wanted.is_some() {
                w::loading_indicator(ui, rect.center(), 22.0);
            } else {
                w::empty_state(ui, rect, empty.0, empty.1, empty.2);
            }
            return;
        };
        let (left, right) = self.split_files(ui, "detail-split", rect);
        self.files_card(ui, left, &detail);
        self.diff_card(ui, right);
    }

    fn files_card(&mut self, ui: &mut Ui, rect: Rect, detail: &Detail) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        w::card(ui, rect, c.surface, CARD);
        let inner = rect.shrink(10.0);
        let p = ui.painter().with_clip_rect(inner);
        let mut y = inner.top();

        if let Some(commit) = &detail.commit {
            w::avatar(&p, pos2(inner.left() + 22.0, y + 24.0), 17.0, &commit.author, &c);
            let text = Rect::from_min_max(pos2(inner.left() + 48.0, y + 4.0), pos2(inner.right(), y + 24.0));
            w::label_in(&p, text, &commit.author, theme::emph(14.0), c.on_surface);
            let sub = format!("{}  \u{00b7}  {}", commit.email, commit.date);
            w::label_in(&p, text.translate(vec2(0.0, 20.0)), &sub, theme::body(12.0), c.on_surface_variant);
            y += 54.0;
            let sha = Rect::from_min_size(pos2(inner.left() + 6.0, y), vec2(104.0, 24.0));
            p.rect_filled(sha, w::radius(12.0), c.surface_highest);
            p.text(sha.center(), Align2::CENTER_CENTER, short(&commit.sha), theme::mono(12.0), c.on_surface_variant);
            let copy = Rect::from_min_size(pos2(sha.right() + 4.0, y - 2.0), vec2(28.0, 28.0));
            if w::icon_btn(ic::COPY, "Copy SHA").size(28.0).show_at(ui, self.id("copy-sha"), copy).clicked() {
                ctx.copy_text(commit.sha.clone());
            }
            let mut x = copy.right() + 8.0;
            for parent in &commit.parents {
                let link = Rect::from_min_size(pos2(x, y), vec2(84.0, 24.0));
                let resp = ui.interact(link, self.id("parent").with(parent), Sense::click()).on_hover_text("Go to parent commit");
                p.rect_stroke(link, w::radius(12.0), Stroke::new(1.0, c.outline_variant), egui::StrokeKind::Inside);
                p.text(link.center(), Align2::CENTER_CENTER, &parent[..parent.len().min(8)], theme::mono(12.0), if resp.hovered() { c.primary } else { c.on_surface_variant });
                if resp.clicked() && let Some(snap) = self.snap.clone() {
                    self.jump_to(&ctx, &snap, parent);
                }
                x += 90.0;
            }
            y += 34.0;
        } else if let Some(stash) = &detail.stash {
            w::cookie(&p, pos2(inner.left() + 22.0, y + 24.0), 18.0, 6.0, 0.08, 0.0, c.tertiary_container);
            w::icon_at(&p, pos2(inner.left() + 22.0, y + 24.0), ic::STASH, 18.0, c.on_tertiary_container);
            let text = Rect::from_min_max(pos2(inner.left() + 48.0, y + 4.0), pos2(inner.right(), y + 24.0));
            w::label_in(&p, text, &stash.name, theme::emph(14.0), c.on_surface);
            w::label_in(&p, text.translate(vec2(0.0, 20.0)), minutes(&stash.date), theme::body(12.0), c.on_surface_variant);
            y += 54.0;
            let actions = Rect::from_min_size(pos2(inner.left() + 2.0, y), vec2(inner.width(), 36.0));
            let name = stash.name.clone();
            w::hchild(ui, "stash-actions", actions, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if w::btn("Apply").kind(Kind::Tonal).height(32.0).show(ui).clicked() {
                    self.run(&ctx, vec![Op::StashApply { name: name.clone(), pop: false }]);
                }
                if w::btn("Pop").kind(Kind::Tonal).height(32.0).show(ui).clicked() {
                    self.run(&ctx, vec![Op::StashApply { name: name.clone(), pop: true }]);
                }
                if w::btn("Drop").kind(Kind::Text).height(32.0).show(ui).clicked() {
                    self.dialog = Some(dialogs::drop_stash(name.clone(), &stash.message));
                }
            });
            y += 44.0;
        }

        // Message, scrollable when long.
        let message = if detail.message.is_empty() { "(no message)" } else { detail.message.as_str() };
        let galley = p.layout(message.to_owned(), theme::body(13.5), c.on_surface, inner.width() - 16.0);
        let message_h = galley.size().y.min((inner.height() * 0.3).max(40.0));
        let message_rect = Rect::from_min_size(pos2(inner.left() + 8.0, y), vec2(inner.width() - 16.0, message_h));
        w::child(ui, "detail-message", message_rect, |ui| {
            ScrollArea::vertical().id_salt("detail-message").auto_shrink(false).show(ui, |ui| ui.label(galley));
        });
        y += message_h + 10.0;

        let count = format!("{} file{} changed", detail.files.len(), if detail.files.len() == 1 { "" } else { "s" });
        p.text(pos2(inner.left() + 8.0, y + 10.0), Align2::LEFT_CENTER, count, theme::emph(12.5), c.on_surface_variant);
        y += 24.0;

        let list = Rect::from_min_max(pos2(inner.left(), y), inner.max);
        w::rows(ui, "detail-files", list, ROW, detail.files.len(), None, |ui, range| {
            for i in range {
                let change = &detail.files[i];
                let (row, resp) = w::row(ui, ROW);
                let selected = self.detail_file == Some(i);
                w::row_bg(ui, row, &resp, selected);
                paint_change_row(ui.painter(), &c, row, change, change.index, selected, 0.0);
                if resp.clicked() {
                    self.pick_file(&ctx, i, change.clone());
                }
                resp.context_menu(|ui| {
                    if w::menu_item(ui, ic::HISTORY, "File history") {
                        self.file_history(&ctx, &change.path);
                    }
                    if w::menu_item(ui, ic::PERSON, "Blame at this commit") {
                        self.blame(&ctx, detail.sha.clone(), change.path.clone());
                    }
                    if w::menu_item(ui, ic::COPY, "Copy path") {
                        ctx.copy_text(change.path.clone());
                    }
                });
            }
        });
    }

    fn diff_card(&mut self, ui: &mut Ui, rect: Rect) {
        let c = w::colors(ui);
        w::card(ui, rect, c.surface, CARD);
        let inner = rect.shrink(8.0);
        if self.diff_spec.is_none() {
            w::empty_state(ui, inner, ic::FILE, "No file selected", "Pick a file to see its changes");
            return;
        }
        let Some(diff) = self.diff.clone() else {
            w::loading_indicator(ui, inner.center(), 20.0);
            return;
        };
        let p = ui.painter().with_clip_rect(inner);
        let header = Rect::from_min_size(inner.min, vec2(inner.width(), 36.0));
        let (dir, name) = split_path(&self.diff_title);
        let mut x = header.left() + 10.0;
        x += w::label_in(&p, Rect::from_min_max(pos2(x, header.top()), header.max - vec2(150.0, 0.0)), name, theme::emph(14.0), c.on_surface) + 10.0;
        w::label_in(&p, Rect::from_min_max(pos2(x, header.top()), header.max - vec2(110.0, 0.0)), dir, theme::body(12.5), c.on_surface_variant);
        let stats = p.text(pos2(header.right() - 10.0, header.center().y), Align2::RIGHT_CENTER, format!("\u{2212}{}", diff.deleted), theme::emph(13.0), c.deleted);
        p.text(pos2(stats.left() - 8.0, header.center().y), Align2::RIGHT_CENTER, format!("+{}", diff.added), theme::emph(13.0), c.added);

        let body = Rect::from_min_max(pos2(inner.left(), header.bottom() + 4.0), inner.max);
        if diff.lines.is_empty() {
            let (title, hint) = if diff.binary { ("Binary file", "No text preview for this change") } else { ("No text changes", "Only metadata changed (mode, rename)") };
            w::empty_state(ui, body, ic::FILE, title, hint);
            return;
        }
        w::child(ui, "diff-body", body, |ui| paint_diff(ui, &c, &diff));
    }

    // ------------------------------------------------------------ changes

    fn changes_page(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        let (left, right) = self.split_files(ui, "changes-split", rect);
        w::card(ui, left, c.surface, CARD);
        let mut inner = left.shrink(10.0);

        if snap.state != InProgress::None {
            let banner = Rect::from_min_size(inner.min, vec2(inner.width(), 84.0));
            self.progress_banner(ui, banner, snap);
            inner.min.y = banner.bottom() + 8.0;
        }
        if snap.changes.is_empty() {
            w::empty_state(ui, inner, ic::CHECK_CIRCLE, "Working tree clean", "Nothing to commit");
        } else {
            let unstaged: Vec<&Change> = snap.changes.iter().filter(|ch| ch.unstaged()).collect();
            let staged: Vec<&Change> = snap.changes.iter().filter(|ch| ch.staged()).collect();
            let top = Rect::from_min_size(inner.min, vec2(inner.width(), inner.height() * 0.55 - 4.0));
            let bottom = Rect::from_min_max(pos2(inner.left(), top.bottom() + 8.0), inner.max);
            self.change_list(ui, top, snap, &unstaged, false);
            self.change_list(ui, bottom, snap, &staged, true);
        }

        let commit_h = 178.0;
        let diff = Rect::from_min_max(right.min, pos2(right.right(), right.bottom() - commit_h - GAP));
        let commit = Rect::from_min_max(pos2(right.left(), diff.bottom() + GAP), right.max);
        self.diff_card(ui, diff);

        w::card(ui, commit, c.surface, CARD);
        let inner = commit.shrink(10.0);
        let actions = Rect::from_min_max(pos2(inner.left(), inner.bottom() - 40.0), inner.max);
        let area = Rect::from_min_max(inner.min, pos2(inner.right(), actions.top() - 8.0));
        let hint = if self.amend { "Commit message (empty keeps the previous one)" } else { "Commit message" };
        let field = w::text_area(ui, "commit-message", area, &mut self.message, hint);
        let has_staged = snap.changes.iter().any(Change::staged);
        let ready = self.busy.is_none() && (self.amend || (has_staged && !self.message.trim().is_empty()));
        let commit_op = Op::Commit { message: self.message.clone(), amend: self.amend, signoff: self.signoff };
        if ready && field.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter)) {
            self.run(&ctx, vec![commit_op.clone()]);
        }
        w::hchild(ui, "commit-actions", actions, |ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.add_space(4.0);
            if w::switch(ui, &mut self.amend, "Amend").changed() && self.amend && self.message.trim().is_empty() {
                self.spawn(&ctx, |repo| Msg::LastMessage(git::full_message(repo, "HEAD")));
            }
            w::switch(ui, &mut self.signoff, "Sign off");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if w::btn("Commit").icon(ic::CHECK).enabled(ready).show(ui).clicked() {
                    self.run(&ctx, vec![commit_op.clone()]);
                }
                if w::btn("Commit & push").icon(ic::PUSH).kind(Kind::Tonal).enabled(ready).show(ui).clicked() {
                    match dialogs::push_op(snap) {
                        Ok(push) => self.run(&ctx, vec![commit_op.clone(), push]),
                        Err(reason) => self.notify(reason, true, &ctx),
                    }
                }
            });
        });
    }

    fn progress_banner(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        w::card(ui, rect, c.tertiary_container, shape::LARGE);
        let p = ui.painter();
        w::icon_at(p, pos2(rect.left() + 22.0, rect.top() + 22.0), ic::WARNING, 20.0, c.on_tertiary_container);
        let conflicts = snap.changes.iter().filter(|ch| ch.conflicted()).count();
        let text = if conflicts > 0 { format!("{} in progress \u{00b7} {conflicts} conflict(s)", snap.state.label()) } else { format!("{} in progress", snap.state.label()) };
        p.text(pos2(rect.left() + 40.0, rect.top() + 22.0), Align2::LEFT_CENTER, text, theme::emph(13.5), c.on_tertiary_container);
        let state = snap.state;
        let actions = Rect::from_min_max(pos2(rect.left() + 10.0, rect.top() + 42.0), rect.max - vec2(10.0, 8.0));
        w::hchild(ui, "progress-actions", actions, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if w::btn("Continue").icon(ic::PLAY).height(32.0).enabled(conflicts == 0).show(ui).clicked() {
                self.run(&ctx, vec![Op::Continue(state)]);
            }
            if state == InProgress::Rebase && w::btn("Skip").icon(ic::SKIP).kind(Kind::Text).height(32.0).show(ui).clicked() {
                self.run(&ctx, vec![Op::Skip]);
            }
            if w::btn("Abort").icon(ic::CANCEL).kind(Kind::Text).height(32.0).show(ui).clicked() {
                self.run(&ctx, vec![Op::Abort(state)]);
            }
        });
    }

    fn change_list(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot, changes: &[&Change], staged: bool) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
        let (title, action) = if staged { ("Staged", (ic::REMOVE, "Unstage all")) } else { ("Unstaged", (ic::STAGE_ALL, "Stage all")) };
        let clicked = w::child(ui, title, header, |ui| w::section_header(ui, title, changes.len(), None, (!changes.is_empty()).then_some(action)));
        if clicked {
            self.run(&ctx, vec![if staged { Op::UnstageAll } else { Op::StageAll }]);
        }
        let toggle = |change: &Change| -> Op {
            let mut paths = vec![change.path.clone()];
            paths.extend(change.orig.clone());
            if staged { Op::Unstage { paths } } else { Op::Stage { paths } }
        };
        let list = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        w::rows(ui, if staged { "staged-rows" } else { "unstaged-rows" }, list, ROW, changes.len(), None, |ui, range| {
            for i in range {
                let change = changes[i];
                let (row, resp) = w::row(ui, ROW);
                let selected = self.picked.as_ref().is_some_and(|(path, s)| *s == staged && path == &change.path);
                w::row_bg(ui, row, &resp, selected);
                let status = match (staged, change.conflicted(), change.untracked()) {
                    (true, ..) => change.index,
                    (_, true, _) => 'U',
                    (_, _, true) => '?',
                    _ => change.work,
                };
                let hot = ui.rect_contains_pointer(row);
                paint_change_row(ui.painter(), &c, row, change, status, selected, if hot { 34.0 } else { 0.0 });
                if resp.double_clicked() {
                    self.run(&ctx, vec![toggle(change)]);
                } else if resp.clicked() {
                    self.pick_change(&ctx, change, staged);
                }
                if hot {
                    let button = Rect::from_center_size(pos2(row.right() - 20.0, row.center().y), vec2(26.0, 26.0));
                    let (icon, tip) = if staged { (ic::REMOVE, "Unstage") } else { (ic::ADD, "Stage") };
                    if w::icon_btn(icon, tip).size(26.0).kind(Kind::Tonal).show_at(ui, resp.id.with("toggle"), button).clicked() {
                        self.run(&ctx, vec![toggle(change)]);
                    }
                }
                resp.context_menu(|ui| {
                    if w::menu_item(ui, if staged { ic::REMOVE } else { ic::ADD }, if staged { "Unstage" } else { "Stage" }) {
                        self.run(&ctx, vec![toggle(change)]);
                    }
                    if change.conflicted() {
                        for (label, theirs) in [("Resolve using ours", false), ("Resolve using theirs", true)] {
                            if w::menu_item(ui, ic::MERGE, label) {
                                self.run(&ctx, vec![Op::Resolve { path: change.path.clone(), theirs }]);
                            }
                        }
                    }
                    w::menu_separator(ui);
                    if w::menu_item(ui, ic::HISTORY, "File history") {
                        self.file_history(&ctx, &change.path);
                    }
                    if !change.untracked() && w::menu_item(ui, ic::PERSON, "Blame") {
                        self.blame(&ctx, String::new(), change.path.clone());
                    }
                    if w::menu_item(ui, ic::COPY, "Copy path") {
                        ctx.copy_text(change.path.clone());
                    }
                    if !staged {
                        w::menu_separator(ui);
                        if w::menu_danger(ui, ic::UNDO, "Discard changes\u{2026}") {
                            self.dialog = Some(dialogs::discard(vec![change.clone()]));
                        }
                        if w::menu_danger(ui, ic::DELETE, "Discard all unstaged changes\u{2026}") {
                            let all = snap.changes.iter().filter(|ch| ch.unstaged() && !ch.conflicted()).cloned().collect();
                            self.dialog = Some(dialogs::discard(all));
                        }
                    }
                });
            }
        });
    }

    // ------------------------------------------------------------ stashes

    fn stashes_page(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        let (list, detail) = self.list_and_detail(ui, rect);
        w::card(ui, list, c.surface, CARD);
        if snap.stashes.is_empty() {
            w::empty_state(ui, list, ic::STASH, "No stashes", "Shelve work in progress with Stash in the toolbar");
        }
        w::rows(ui, "stash-rows", list.shrink(8.0), 50.0, snap.stashes.len(), None, |ui, range| {
            for i in range {
                let stash = &snap.stashes[i];
                let (row, resp) = w::row(ui, 50.0);
                let selected = self.stash.as_deref() == Some(stash.sha.as_str());
                w::row_bg(ui, row, &resp, selected);
                let p = ui.painter();
                let fg = if selected { c.on_secondary_container } else { c.on_surface };
                w::icon_at(p, pos2(row.left() + 24.0, row.center().y), ic::STASH, 20.0, c.on_surface_variant);
                let line = Rect::from_min_max(pos2(row.left() + 48.0, row.top() + 6.0), pos2(row.right() - 12.0, row.top() + 26.0));
                w::label_in(p, line, &stash.message, theme::emph(13.5), fg);
                let sub = format!("{}  \u{00b7}  {}", stash.name, minutes(&stash.date));
                w::label_in(p, line.translate(vec2(0.0, 19.0)), &sub, theme::body(12.0), c.on_surface_variant);
                if resp.clicked() {
                    self.select_stash(&ctx, stash);
                }
                resp.context_menu(|ui| {
                    for (label, pop) in [("Apply", false), ("Pop", true)] {
                        if w::menu_item(ui, ic::STASH, label) {
                            self.run(&ctx, vec![Op::StashApply { name: stash.name.clone(), pop }]);
                        }
                    }
                    w::menu_separator(ui);
                    if w::menu_danger(ui, ic::DELETE, "Drop\u{2026}") {
                        self.dialog = Some(dialogs::drop_stash(stash.name.clone(), &stash.message));
                    }
                });
            }
        });
        self.detail_cards(ui, detail, (ic::STASH, "No stash selected", "Pick a stash to see what it holds"));
    }

    // ------------------------------------------------------------ overlays

    fn overlays(&mut self, ctx: &Context) {
        if let Some(dialog) = &mut self.dialog {
            match dialog.show(ctx) {
                Outcome::Open => {}
                Outcome::Cancelled => self.dialog = None,
                Outcome::Confirmed(ops) => {
                    self.dialog = None;
                    self.run(ctx, ops);
                }
            }
        }
        if self.show_log {
            let text = if self.log.is_empty() { "No commands run yet." } else { self.log.as_str() };
            self.show_log = !text_dialog(ctx, "log-dialog", ic::TERMINAL, "Command log", text, true);
        }
        if let Some((title, text)) = &self.text_view
            && text_dialog(ctx, "text-dialog", ic::PERSON, title, text, false)
        {
            self.text_view = None;
        }
        let Some(notice) = &self.notice else { return };
        let error = notice.error;
        // Expire before drawing: no repaint follows, so the last painted frame must not show it.
        if !error && ctx.input(|i| i.time) - notice.at > 4.0 {
            self.notice = None;
            return;
        }
        let text: String = notice.text.lines().take(4).collect::<Vec<_>>().join("\n");
        let (details, dismissed) = w::snackbar(ctx, "repo-snackbar", &text, error, if error { "Log" } else { "" });
        self.show_log |= details;
        if details || dismissed {
            self.notice = None;
        } else if !error {
            ctx.request_repaint_after_secs(0.5);
        }
    }
}

/// Scrollable monospace text in a dialog. Returns true when closed.
fn text_dialog(ctx: &Context, id: &str, icon: &str, title: &str, text: &str, stick_to_bottom: bool) -> bool {
    let c = theme::colors(ctx);
    let (closed, dismissed) = w::dialog(ctx, id, icon, title, 860.0, |ui| {
        let frame = egui::Frame::new().fill(c.surface_low).corner_radius(w::radius(shape::LARGE)).inner_margin(egui::Margin::same(12));
        frame.show(ui, |ui| {
            ScrollArea::both().max_height(460.0).auto_shrink(false).stick_to_bottom(stick_to_bottom).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(text).font(theme::mono(12.5)).color(c.on_surface)).extend());
            });
        });
        ui.add_space(20.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| w::btn("Close").kind(Kind::Tonal).show(ui).clicked()).inner
    });
    closed || dismissed
}

// ---------------------------------------------------------------- painting

fn side_row(ui: &mut Ui, c: &Colors, indent: usize, icon: &str, tint: Color32, text: &str, emphasized: bool, trailing: &str) -> Response {
    let (row, resp) = w::row(ui, ROW);
    w::row_bg(ui, row, &resp, false);
    let p = ui.painter();
    let left = row.left() + 26.0 + indent as f32 * 18.0;
    w::icon_at(p, pos2(left, row.center().y), icon, 16.0, tint);
    let trailing_w = if trailing.is_empty() { 0.0 } else { w::text_width(p, trailing, theme::body(12.0)) + 10.0 };
    let font = if emphasized { theme::emph(13.5) } else { theme::body(13.5) };
    w::label_in(p, Rect::from_min_max(pos2(left + 14.0, row.top()), pos2(row.right() - 8.0 - trailing_w, row.bottom())), text, font, c.on_surface);
    if !trailing.is_empty() {
        p.text(pos2(row.right() - 10.0, row.center().y), Align2::RIGHT_CENTER, trailing, theme::body(12.0), c.on_surface_variant);
    }
    resp
}

/// Status badge, file name, then its directory in a quieter tone.
fn paint_change_row(p: &Painter, c: &Colors, row: Rect, change: &Change, status: char, selected: bool, reserve_right: f32) {
    w::status_badge(p, pos2(row.left() + 18.0, row.center().y), status, c);
    let (dir, name) = split_path(&change.path);
    let fg = if selected { c.on_secondary_container } else { c.on_surface };
    let right = row.right() - 8.0 - reserve_right;
    let mut x = row.left() + 36.0;
    x += w::label_in(p, Rect::from_min_max(pos2(x, row.top()), pos2(right, row.bottom())), name, theme::emph(13.0), fg) + 8.0;
    let dir = match &change.orig {
        Some(orig) => format!("{dir}  \u{2190} {orig}"),
        None => dir.to_string(),
    };
    w::label_in(p, Rect::from_min_max(pos2(x, row.top()), pos2(right, row.bottom())), &dir, theme::body(12.0), c.on_surface_variant);
}

fn paint_commit_row(p: &Painter, c: &Colors, row: Rect, commit: &Commit, margin: f32, selected: bool) {
    let fg = if selected { c.on_secondary_container } else { c.on_surface };
    let dim = if selected { c.on_secondary_container } else { c.on_surface_variant };
    let mid = row.center().y;
    let date = Rect::from_min_max(pos2(row.right() - 124.0, row.top()), pos2(row.right() - 10.0, row.bottom()));
    let sha = Rect::from_min_size(pos2(date.left() - 88.0, row.top()), vec2(84.0, row.height()));
    let author_w = if row.width() > 760.0 { 150.0 } else { 0.0 };
    let author = Rect::from_min_size(pos2(sha.left() - author_w - 8.0, row.top()), vec2(author_w, row.height()));
    w::label_in(p, date, &commit.date, theme::body(12.5), dim);
    w::label_in(p, sha, short(&commit.sha), theme::mono(12.0), dim);
    if author_w > 0.0 {
        w::avatar(p, pos2(author.left() + 9.0, mid), 9.0, &commit.author, c);
        w::label_in(p, author.with_min_x(author.left() + 24.0), &commit.author, theme::body(13.0), dim);
    }

    let mut x = row.left() + 4.0 + margin + 6.0;
    let subject_right = author.left() - 10.0;
    for r in &commit.refs {
        if x > subject_right - 140.0 {
            break;
        }
        let (icon, fill, on) = match r.kind {
            RefKind::Head => (ic::CHECK, c.primary, c.on_primary),
            RefKind::Local => (ic::BRANCH, c.secondary_container, c.on_secondary_container),
            RefKind::Remote => (ic::CLOUD, c.surface_highest, c.on_surface_variant),
            RefKind::Tag => (ic::TAG, c.tertiary_container, c.on_tertiary_container),
        };
        x += w::chip(p, pos2(x, mid), icon, &r.name, fill, on) + 5.0;
    }
    let font = if commit.is_head { theme::emph(13.5) } else { theme::body(13.5) };
    w::label_in(p, Rect::from_min_max(pos2(x, row.top()), pos2(subject_right, row.bottom())), &commit.subject, font, fg);
}

fn paint_graph(p: &Painter, graph: &Graph, c: &Colors, origin: Pos2, rows: &Range<usize>) {
    let (top, bottom) = (rows.start as f32, rows.end as f32);
    let at = |(x, y): graph::Point| pos2(origin.x + x, origin.y + y * ROW);
    let stroke = |color: usize| Stroke::new(2.0, c.lanes[color % c.lanes.len()]);
    let quad = |points: [Pos2; 3], s: Stroke| {
        p.add(QuadraticBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, s));
    };

    for link in graph.links.iter().take_while(|l| l.start.1 <= bottom).filter(|l| l.end.1 >= top) {
        quad([at(link.start), at(link.control), at(link.end)], stroke(link.color));
    }
    for lane in graph.lanes.iter().take_while(|l| l.points[0].1 <= bottom) {
        let s = stroke(lane.color);
        for (i, pair) in lane.points.windows(2).enumerate() {
            if pair[1].1 < top {
                continue;
            }
            if pair[0].1 > bottom {
                break;
            }
            let (a, b) = (at(pair[0]), at(pair[1]));
            let last = i + 2 == lane.points.len();
            if b.x > a.x {
                quad([a, pos2(b.x, a.y), b], s);
            } else if b.x < a.x && last {
                quad([a, pos2(a.x, b.y), b], s);
            } else if b.x < a.x {
                let mid = (a.y + b.y) / 2.0;
                p.add(CubicBezierShape::from_points_stroke([a, pos2(a.x, mid + 4.0), pos2(b.x, mid - 4.0), b], false, Color32::TRANSPARENT, s));
            } else {
                p.line_segment([a, b], s);
            }
        }
    }
    for dot in rows.clone().filter_map(|i| graph.dots.get(i)) {
        let (center, color) = (at(dot.center), c.lanes[dot.color % c.lanes.len()]);
        match dot.kind {
            DotKind::Normal => {
                p.circle_filled(center, 5.5, c.surface);
                p.circle_filled(center, 4.0, color);
            }
            DotKind::Head => {
                p.circle_filled(center, 7.0, color);
                p.circle_filled(center, 3.0, c.surface);
            }
            DotKind::Merge => {
                p.circle_filled(center, 5.5, c.surface);
                p.circle_stroke(center, 4.0, Stroke::new(2.0, color));
            }
        }
    }
}

fn paint_diff(ui: &mut Ui, c: &Colors, diff: &Diff) {
    const LINE: f32 = 20.0;
    let font = theme::mono(12.5);
    let char_w = w::text_width(ui.painter(), "0", font.clone());
    let digits = diff.lines.iter().rev().find_map(|l| l.new.or(l.old)).unwrap_or(1).to_string().len().max(3) as f32;
    let gutter = (digits * 2.0 + 3.0) * char_w + 16.0;
    let content_w = gutter + (diff.max_chars as f32 + 4.0) * char_w;
    ui.spacing_mut().item_spacing.y = 0.0;
    ScrollArea::both().id_salt("diff-scroll").auto_shrink(false).show_rows(ui, LINE, diff.lines.len(), |ui, range| {
        ui.set_min_width(content_w);
        let width = content_w.max(ui.available_width());
        for line in &diff.lines[range] {
            let (row, _) = ui.allocate_exact_size(vec2(width, LINE), Sense::hover());
            let p = ui.painter();
            let (bg, sign, fg) = match line.kind {
                LineKind::Added => (c.diff_added, "+", c.on_surface),
                LineKind::Deleted => (c.diff_deleted, "\u{2212}", c.on_surface),
                LineKind::Hunk => (c.surface_highest, "", c.on_surface_variant),
                LineKind::Context => (Color32::TRANSPARENT, "", c.on_surface),
                LineKind::Note => (Color32::TRANSPARENT, "", c.on_surface_variant),
            };
            let mid = row.center().y;
            if line.kind == LineKind::Hunk {
                p.rect_filled(row.shrink2(vec2(4.0, 1.0)), w::radius(shape::SMALL), bg);
                p.text(pos2(row.left() + 14.0, mid), Align2::LEFT_CENTER, &line.text, font.clone(), fg);
                continue;
            }
            p.rect_filled(row, 0.0, bg);
            let number = |n: Option<u32>, right: f32| {
                if let Some(n) = n {
                    p.text(pos2(right, mid), Align2::RIGHT_CENTER, n, font.clone(), c.on_surface_variant.gamma_multiply(0.7));
                }
            };
            number(line.old, row.left() + 8.0 + digits * char_w);
            number(line.new, row.left() + 8.0 + (digits * 2.0 + 1.0) * char_w);
            p.text(pos2(row.left() + gutter - 2.0 * char_w, mid), Align2::LEFT_CENTER, sign, font.clone(), fg);
            p.text(pos2(row.left() + gutter, mid), Align2::LEFT_CENTER, &line.text, font.clone(), fg);
        }
    });
}

/// `take` count for the rows of a collapsible section.
fn visible(open: bool) -> usize {
    if open { usize::MAX } else { 0 }
}
