//! One open repository: state, background work, and its three pages
//! (history, local changes, stashes).

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui::epaint::{CubicBezierShape, QuadraticBezierShape};
use eframe::egui::{self, Align, Align2, Color32, Context, Id, Key, Layout, Painter, Pos2, Rect, Response, ScrollArea, Sense, Stroke, Ui, ViewportBuilder, ViewportCommand, ViewportId, pos2, vec2};

use crate::conflict::{self, FileState};
use crate::dialogs::{self, Dialog, Outcome};
use crate::diff::{self, Diff, LineKind};
use crate::git::{self, Branch, Change, Commit, ConflictSide, DiffSpec, InProgress, LogQuery, Op, OpResult, RefKind, SearchBy, Snapshot, Stash, short};
use crate::graph::{self, DotKind, Graph};
use crate::merge_editor::{Action, MergeEditor};
use crate::open_in;
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

/// Kinds of sidebar rows that can be multi-selected. One kind at a time.
#[derive(Clone, Copy, PartialEq)]
enum Group {
    Local,
    Remote,
    Tag,
    Worktree,
}

/// One row of a file tree: in the details card and in the lists of changes.
enum FileRow {
    Folder { key: String, name: String, depth: usize, open: bool },
    /// Index into the files the tree was built from.
    File { index: usize, depth: usize },
}

/// Lays changed files out by folder, the way the sidebar lays out branches. Every
/// folder is open unless it is in `folded`, where its key starts with `scope`.
fn file_rows<'a>(files: impl IntoIterator<Item = &'a Change>, scope: &str, folded: &BTreeSet<String>) -> Vec<FileRow> {
    let paths: Vec<&str> = files.into_iter().map(|f| f.path.as_str()).collect();
    let rows = tree_rows(&paths, scope, &|key| !folded.contains(key));
    rows.into_iter()
        .map(|row| match row {
            TreeRow::Folder { key, name, depth, open } => FileRow::Folder { key, name: name.to_string(), depth, open },
            TreeRow::Leaf { index, depth, .. } => FileRow::File { index, depth },
        })
        .collect()
}

/// The lines picked in the diff of a change, to stage or unstage on their own.
#[derive(Default)]
struct LinePick {
    /// The picked lines, as indices into the diff's lines. Only changed lines are picked.
    lines: BTreeSet<usize>,
    /// The line clicked last, where a Shift-click range starts.
    anchor: Option<usize>,
    /// A drag in progress: the line it began on, whether it picks or lets go, and
    /// what was picked before it.
    drag: Option<(usize, bool, BTreeSet<usize>)>,
}

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
    /// The external diff program, by name, has closed (or could not be started).
    Tool(String, OpResult),
    /// The conflict editor's file was written and staged (or that failed).
    Resolved(OpResult),
    /// What the conflict panel shows for a file.
    Sides(String, ConflictInfo),
    /// A background task panicked; details are in the crash log.
    Crashed,
}

/// Both sides of the conflict over a file, and what can be done with the file.
#[derive(Clone)]
struct ConflictInfo {
    current: ConflictSide,
    incoming: ConflictSide,
    state: FileState,
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
    /// The conflict editor, in its own window while a file is being resolved.
    merge: Option<MergeEditor>,
    /// After a file is resolved, open the next conflicted one when the snapshot arrives.
    merge_next: bool,
    /// The conflict panel's data for a file (its path first); `None` while loading.
    sides: Option<(String, Option<ConflictInfo>)>,
    /// The conflict panel's checkboxes: take current, take incoming.
    take: (bool, bool),
    /// Branch folders left open, by key: `local/feat/anims`, `remote/origin`, `remote/origin/fix`.
    /// Folders start closed, so a repository with many branches opens calm.
    pub opened: BTreeSet<String>,
    /// Colors given to branch folders: the folder (`feat`, `feat/menu`) and its hue.
    /// The same folder under a remote wears the same color.
    pub folder_colors: BTreeMap<String, f32>,
    /// A color being tried on: the folder, and the hue of the swatch under the pointer.
    color_preview: Option<(String, f32)>,
    /// `folder_colors` as shown this frame, the color being tried on included.
    tints: BTreeMap<String, f32>,
    /// Multi-selected sidebar rows of `mark_group`: branch or tag names, worktree paths.
    marked: BTreeSet<String>,
    mark_group: Group,
    /// Where a Shift-click range starts.
    anchor: Option<String>,
    selected: Option<String>,
    scroll_to: Option<usize>,
    wanted: Option<String>,
    detail: Option<Arc<Detail>>,
    detail_file: Option<usize>,
    /// Folders folded in the details' file tree, by key: `files/src/kitchen`.
    folded: BTreeSet<String>,
    /// The file tree's rows, rebuilt when the details or `folded` change.
    file_rows: Option<Vec<FileRow>>,
    /// Folders folded in the lists of changes: `unstaged/src/kitchen`, `staged/docs`.
    change_folds: BTreeSet<String>,
    picked: Option<(String, bool)>,
    message: String,
    amend: bool,
    signoff: bool,
    stash: Option<String>,
    diff_generation: u64,
    diff_spec: Option<DiffSpec>,
    diff: Option<Arc<Diff>>,
    line_pick: LinePick,
    diff_title: String,
    /// Pane sizes and open sections; the app copies the shared layout in each frame.
    pub layout: crate::prefs::Layout,
    /// The diff program picked in Settings: its name and the command git runs. The app
    /// sets it each frame, like the layout.
    pub diff_tool: Option<(String, String)>,
}

/// `2026-01-31 12:00:59 +0100` -> `2026-01-31 12:00`, matching commit dates.
fn minutes(date: &str) -> &str {
    date.get(..16).unwrap_or(date)
}

fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('/').map_or(("", path), |(dir, name)| (dir, name))
}

impl RepoView {
    pub fn new(path: PathBuf, history_limit: usize, opened: BTreeSet<String>, ctx: &Context) -> Self {
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
            merge: None,
            merge_next: false,
            sides: None,
            take: (true, true),
            opened,
            folder_colors: BTreeMap::new(),
            color_preview: None,
            tints: BTreeMap::new(),
            marked: BTreeSet::new(),
            mark_group: Group::Local,
            anchor: None,
            selected: None,
            scroll_to: None,
            wanted: None,
            detail: None,
            detail_file: None,
            folded: BTreeSet::new(),
            file_rows: None,
            change_folds: BTreeSet::new(),
            picked: None,
            message: String::new(),
            amend: false,
            signoff: false,
            stash: None,
            diff_generation: 0,
            diff_spec: None,
            diff: None,
            line_pick: LinePick::default(),
            diff_title: String::new(),
            layout: crate::prefs::Layout::default(),
            diff_tool: None,
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
            // A panic here must not leave the UI waiting forever; the hook has logged it.
            let msg = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&path))).unwrap_or(Msg::Crashed);
            let _ = tx.send(msg); // receiver gone = tab closed
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

    /// Runs dependent steps: the first failure stops the rest.
    fn run(&mut self, ctx: &Context, ops: Vec<Op>) {
        self.start(ctx, ops, false);
    }

    /// `keep_going`: the operations are independent (a batch), so run them all.
    fn start(&mut self, ctx: &Context, ops: Vec<Op>, keep_going: bool) {
        let Some(first) = ops.first() else { return };
        if let Some(running) = self.busy {
            self.notify(format!("{running} is still running"), true, ctx);
            return;
        }
        let title = first.title();
        self.busy = Some(title);
        self.spawn(ctx, move |repo| Msg::Done(title, git::run_ops(repo, &ops, keep_going)));
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
                    // Open on the file at the top of the tree, which lists folders first.
                    let rows = file_rows(&detail.files, "files", &self.folded);
                    let first = rows.iter().find_map(|row| if let FileRow::File { index, .. } = row { Some(*index) } else { None });
                    let first = first.map(|index| (index, detail.files[index].clone()));
                    self.detail = Some(Arc::new(detail));
                    self.detail_file = None;
                    self.file_rows = Some(rows);
                    if let Some((index, change)) = first {
                        self.pick_file(ctx, index, change);
                    }
                }
                Msg::Diff(generation, diff) if generation == self.diff_generation => {
                    // Picked lines are places in the diff: they hold only while it reads the same.
                    if self.diff.as_ref().is_none_or(|shown| shown.raw != diff.raw) {
                        self.line_pick = LinePick::default();
                    }
                    self.diff = Some(Arc::new(diff));
                }
                Msg::LastMessage(message) if self.message.trim().is_empty() => self.message = message,
                Msg::Text(title, text) => self.text_view = Some((title, text)),
                Msg::Tool(name, result) => {
                    self.log += &result.log;
                    match result.ok {
                        // The file may have been edited in the program.
                        true => self.reload(ctx),
                        false => self.notify(format!("{name} could not be started: {}", result.error.lines().last().unwrap_or("no reason given").trim()), true, ctx),
                    }
                }
                Msg::Resolved(result) => self.on_resolved(ctx, result),
                Msg::Sides(path, info) => {
                    if let Some(editor) = self.merge.as_mut().filter(|e| e.path == path) {
                        editor.set_sides((info.current.clone(), info.incoming.clone()));
                    }
                    if self.sides.as_ref().is_some_and(|(wanted, _)| wanted == &path) {
                        self.sides = Some((path, Some(info)));
                    }
                }
                Msg::Crashed => {
                    (self.busy, self.loading) = (None, false);
                    self.notify("Internal error in a background task. Details are in crash.log in Spoon's settings folder.".into(), true, ctx);
                }
                Msg::Detail(_) | Msg::Diff(..) | Msg::LastMessage(_) => {} // stale
            }
        }
    }

    fn on_done(&mut self, ctx: &Context, title: &'static str, result: OpResult) {
        self.busy = None;
        self.log += &result.log;
        let conflicts = result.log.lines().filter(|line| line.starts_with("CONFLICT")).count();
        if !result.ok && conflicts > 0 {
            // Not a failure: the operation waits for the conflicts to be settled.
            let plural = if conflicts == 1 { "" } else { "s" };
            self.notify(format!("{title} stopped on {conflicts} conflict{plural}. Resolve them here, then continue."), false, ctx);
            self.enter(ctx, Page::Changes);
        } else if !result.ok {
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

    /// Loads the conflict panel's data for a file. Data already shown for the same
    /// file stays on screen until the fresh copy arrives.
    fn load_sides(&mut self, ctx: &Context, change: &Change) {
        if self.sides.as_ref().is_none_or(|(loaded, _)| loaded != &change.path) {
            self.sides = Some((change.path.clone(), None));
        }
        let change = change.clone();
        self.spawn(ctx, move |repo| {
            let (current, incoming) = git::conflict_sides(repo, &change.path);
            let state = conflict::file_state(repo, &change);
            Msg::Sides(change.path, ConflictInfo { current, incoming, state })
        });
    }

    fn merge_window_id(&self) -> ViewportId {
        ViewportId::from_hash_of(("merge", &self.path))
    }

    fn open_conflict(&mut self, ctx: &Context, change: &Change) {
        // One editor per repository: bring it forward rather than drop picked lines.
        if let Some(open) = &self.merge {
            ctx.send_viewport_cmd_to(self.merge_window_id(), ViewportCommand::Focus);
            if open.path == change.path {
                return;
            }
            if open.dirty() {
                return self.notify(format!("Resolve or cancel {} first", open.path), false, ctx);
            }
        }
        let known = self.sides.as_ref().filter(|(loaded, _)| loaded == &change.path).and_then(|(_, info)| info.clone());
        let missing = known.is_none();
        match MergeEditor::open(&self.path, &change.path, known.map(|info| (info.current, info.incoming))) {
            Ok(editor) => self.merge = Some(editor),
            Err(reason) => return self.notify(reason, true, ctx),
        }
        if missing {
            self.load_sides(ctx, change);
        }
    }

    /// Writes the editor's result and stages the file, which marks it resolved.
    fn save_resolution(&mut self, ctx: &Context, content: String) {
        let Some(editor) = &self.merge else { return };
        if let Some(running) = self.busy {
            self.notify(format!("{running} is still running"), true, ctx);
            return;
        }
        let (path, original) = (editor.path.clone(), editor.original.clone());
        self.busy = Some("Resolve conflict");
        self.spawn(ctx, move |repo| {
            Msg::Resolved(match conflict::save(repo, &path, &original, &content) {
                Ok(()) => git::run_op(repo, &Op::Stage { paths: vec![path] }),
                Err(error) => OpResult { ok: false, log: format!("Resolve conflict: {error}\n"), error },
            })
        });
    }

    fn on_resolved(&mut self, ctx: &Context, result: OpResult) {
        self.busy = None;
        self.log += &result.log;
        if result.ok {
            // The editor stays until the snapshot arrives, so its window is reused for the next file.
            self.merge_next = true;
            self.page = Page::Changes;
        } else {
            self.notify(format!("Resolve conflict failed: {}", result.error.trim()), true, ctx);
        }
        self.loading = false;
        self.reload(ctx);
    }

    fn on_snapshot(&mut self, ctx: &Context, snap: Snapshot, graph: Graph) {
        self.loading = false;
        // The editor follows the repository: close it when its file is no longer in
        // conflict (aborted, resolved elsewhere), move on after resolving one here.
        if self.merge.as_ref().is_some_and(|editor| !snap.changes.iter().any(|c| c.conflicted() && c.path == editor.path)) {
            self.merge = None;
        }
        if std::mem::take(&mut self.merge_next) {
            match snap.changes.iter().find(|c| c.conflicted()) {
                Some(next) => self.open_conflict(ctx, next),
                None => self.notify(format!("All conflicts resolved. Continue the {}.", snap.state.label().to_lowercase()), false, ctx),
            }
        }
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
        let group = self.mark_group;
        self.marked.retain(|key| match group {
            Group::Local => snap.branches.iter().any(|b| b.remote.is_none() && &b.name == key),
            Group::Remote => snap.branches.iter().any(|b| b.remote.is_some() && &b.name == key),
            Group::Tag => snap.tags.iter().any(|t| &t.name == key),
            Group::Worktree => snap.worktrees.iter().any(|t| &t.path == key),
        });
        self.snap = Some(Arc::new(snap));
        self.graph = Arc::new(graph);
    }

    // ------------------------------------------------------------ selection

    fn clear_diff(&mut self) {
        self.diff_generation += 1;
        self.diff_spec = None;
        self.diff = None;
        self.line_pick = LinePick::default();
        self.diff_title.clear();
    }

    fn clear_detail(&mut self) {
        self.wanted = None;
        self.detail = None;
        self.detail_file = None;
        self.folded.clear();
        self.file_rows = None;
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

    /// Closes the details, giving the list its full height back.
    fn deselect(&mut self) {
        (self.selected, self.stash) = (None, None);
        self.clear_detail();
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
        if change.conflicted() {
            // The conflict panel takes the diff's place; it shows both sides' history.
            // Reloaded on every refresh: the file may have been fixed in another editor.
            self.clear_diff();
            if self.sides.as_ref().is_none_or(|(loaded, _)| loaded != &change.path) {
                self.take = (true, true);
            }
            self.load_sides(ctx, change);
            return;
        }
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

    /// Whether `change` can go to the external diff program: one is picked, and the
    /// file has an old version to compare with.
    fn can_diff_externally(&self, change: &Change) -> bool {
        self.diff_tool.is_some() && !change.untracked()
    }

    /// Opens the diff in the program picked in Settings, without holding up the window.
    /// Stages the changes in `patch`, or with `reverse` unstages them; the files stay as they are.
    fn stage_lines(&mut self, ctx: &Context, patch: String, reverse: bool) {
        if let Some(running) = self.busy {
            return self.notify(format!("{running} is still running"), true, ctx);
        }
        let title = if reverse { "Unstage lines" } else { "Stage lines" };
        self.busy = Some(title);
        self.spawn(ctx, move |repo| Msg::Done(title, git::apply_to_index(repo, &patch, reverse)));
    }

    fn open_in_diff_tool(&mut self, ctx: &Context, spec: DiffSpec) {
        let Some((name, command)) = self.diff_tool.clone() else { return };
        self.spawn(ctx, move |repo| Msg::Tool(name, git::difftool(repo, &command, &spec)));
    }

    fn blame(&mut self, ctx: &Context, rev: String, path: String) {
        self.spawn(ctx, move |repo| Msg::Text(format!("Blame: {path}"), git::blame(repo, &rev, &path)));
    }

    // ------------------------------------------------------------ layout

    /// The conflict editor's window. Called for every open repository, not only the
    /// active tab, so switching tabs does not close it.
    pub fn merge_window(&mut self, ctx: &Context) {
        let Some(mut editor) = self.merge.take() else { return };
        // Saved and waiting for the snapshot counts as busy: the file on disk already changed.
        let busy = self.busy.is_some() || self.merge_next;
        let builder = ViewportBuilder::default()
            .with_title(format!("{} - Merge - Spoon", editor.path))
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(crate::window_icon());
        let action = ctx.show_viewport_immediate(self.merge_window_id(), builder, |ui, _| {
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, w::colors(ui).surface_container);
            let action = editor.ui(ui, rect, busy);
            // Errors from saving belong where the user is looking.
            self.snackbar(ui.ctx());
            action
        });
        self.merge = Some(editor);
        match action {
            Action::Save(content) => self.save_resolution(ctx, content),
            Action::Close => self.merge = None,
            Action::Stay => {}
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        if ui.input(|i| i.key_pressed(Key::F5)) {
            self.reload(&ctx);
        }
        self.tints.clone_from(&self.folder_colors);
        self.tints.extend(self.color_preview.take());
        // Esc backs out one step: first the sidebar selection, then the open details.
        let overlay = self.dialog.is_some() || self.show_log || self.text_view.is_some();
        if !overlay && ui.input(|i| i.key_pressed(Key::Escape)) {
            if self.page == Page::Changes && !self.line_pick.lines.is_empty() {
                self.line_pick = LinePick::default();
            } else if self.marked.is_empty() && self.page != Page::Changes {
                self.deselect();
            } else {
                self.marked.clear();
            }
        }
        let Some(snap) = self.snap.clone() else {
            w::loading_indicator(ui, rect.center(), 26.0);
            return;
        };
        let rail = Rect::from_min_size(rect.min, vec2(RAIL, rect.height()));
        let content = Rect::from_min_max(pos2(rail.right(), rect.top()), rect.max - vec2(GAP + 2.0, GAP + 2.0));
        self.rail(ui, rail, &snap);

        self.layout.sidebar = self.layout.sidebar.clamp(200.0, (content.width() - 520.0).max(200.0));
        let sidebar = Rect::from_min_size(content.min, vec2(self.layout.sidebar, content.height()));
        let gap = Rect::from_min_size(pos2(sidebar.right(), content.top()), vec2(GAP, content.height()));
        self.layout.sidebar += w::splitter(ui, self.id("sidebar-split"), gap, true);
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
                let open_in = w::btn("Open in").icon(ic::OPEN_IN).kind(Kind::Text).show(ui);
                egui::Popup::menu(&open_in).show(|ui| self.open_in_menu(ui, &ctx, snap));
                if let Some(title) = self.busy {
                    let (spot, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                    w::loading_indicator(ui, spot.center(), 11.0);
                    ui.label(egui::RichText::new(format!("{title}\u{2026}")).font(theme::emph(13.0)).color(c.on_surface_variant));
                }
            });
        });
    }

    /// Terminals and the file manager, the editors found on this machine, and the
    /// repository's page on its remote host.
    fn open_in_menu(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot) {
        let mut editors = false;
        for target in open_in::targets() {
            let icon = match target.kind {
                open_in::Kind::Terminal => ic::TERMINAL,
                open_in::Kind::Files => ic::FOLDER_OPEN,
                open_in::Kind::Editor => ic::CODE,
            };
            if target.kind == open_in::Kind::Editor && !std::mem::replace(&mut editors, true) {
                w::menu_separator(ui);
            }
            if w::menu_item(ui, icon, &target.name)
                && let Err(reason) = target.open(&self.path)
            {
                self.notify(reason, true, ctx);
            }
        }
        let remote = snap.remotes.iter().find(|r| r.name == "origin").or(snap.remotes.first());
        if let Some(web) = remote.and_then(|r| open_in::web_url(&r.url)) {
            w::menu_separator(ui);
            if w::menu_item(ui, ic::WEB, &format!("View on {}", open_in::site_name(&web))) {
                ctx.open_url(egui::OpenUrl::new_tab(web));
            }
        }
    }

    // ------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        w::card(ui, rect, c.surface, CARD);
        let inner = rect.shrink(10.0);
        // A selection bar takes the bottom of the card while rows are marked.
        let bar = if self.marked.is_empty() { 0.0 } else { 48.0 };
        if bar > 0.0 {
            self.selection_bar(ui, Rect::from_min_max(pos2(inner.left(), inner.bottom() - bar), inner.max), &ctx, snap);
        }
        let list = Rect::from_min_max(inner.min, inner.max - vec2(0.0, if bar > 0.0 { bar + 8.0 } else { 0.0 }));
        w::child(ui, "sidebar", list, |ui| {
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

        // While filtering, every folder is shown open so matches are not hidden.
        let filtering = !needle.is_empty();
        let leaves = |rows: &[TreeRow], names: &[&Branch]| -> Vec<String> {
            rows.iter().filter_map(|row| if let TreeRow::Leaf { index, .. } = row { Some(names[*index].name.clone()) } else { None }).collect()
        };

        let locals: Vec<&Branch> = snap.branches.iter().filter(|b| b.remote.is_none() && shown(&b.name)).collect();
        if w::section_header(ui, "Branches", locals.len(), Some(&mut self.layout.sections[0]), Some((ic::ADD, "New branch"))) {
            self.dialog = Some(dialogs::create_branch("HEAD".into(), "the current commit"));
        }
        // The current branch leads the list with its full name, outside the folders,
        // so it is never hunted for.
        let (current, locals): (Vec<&Branch>, Vec<&Branch>) = locals.into_iter().partition(|b| b.current);
        let names: Vec<&str> = locals.iter().map(|b| b.name.as_str()).collect();
        let rows = tree_rows(&names, "local", &|key| filtering || self.opened.contains(key));
        let order: Vec<String> = current.iter().map(|b| b.name.clone()).chain(leaves(&rows, &locals)).collect();
        for branch in current.iter().take(visible(self.layout.sections[0])) {
            self.local_branch_row(ui, ctx, snap, c, branch, &branch.name, 0, &order);
        }
        for row in rows.iter().take(visible(self.layout.sections[0])) {
            match row {
                TreeRow::Leaf { index, name, depth } => self.local_branch_row(ui, ctx, snap, c, locals[*index], name, *depth, &order),
                TreeRow::Folder { key, name, depth, open } => {
                    let members = folder_members(key, "local", &names).into_iter().map(|i| locals[i].name.clone()).collect();
                    self.folder_row(ui, c, key, name, *depth, *open, Group::Local, members);
                }
            }
        }

        if w::section_header(ui, "Remotes", snap.remotes.len(), Some(&mut self.layout.sections[1]), Some((ic::ADD, "Add remote"))) {
            self.dialog = Some(dialogs::add_remote(snap.remotes.is_empty()));
        }
        // Every remote is a folder of its own. All trees are built first, so a
        // Shift-click range can run across remotes.
        struct RemoteTree<'a> {
            remote: &'a git::Remote,
            key: String,
            open: bool,
            branches: Vec<&'a Branch>,
            short: Vec<&'a str>,
            rows: Vec<TreeRow<'a>>,
        }
        let trees: Vec<RemoteTree> = snap
            .remotes
            .iter()
            .map(|remote| {
                let key = format!("remote/{}", remote.name);
                let open = filtering || self.opened.contains(&key);
                let branches: Vec<&Branch> = snap.branches.iter().filter(|b| b.remote.as_deref() == Some(&remote.name) && shown(&b.name)).collect();
                let short: Vec<&str> = branches.iter().map(|b| b.name.get(remote.name.len() + 1..).unwrap_or(&b.name)).collect();
                let rows = if open { tree_rows(&short, &key, &|folder| filtering || self.opened.contains(folder)) } else { Vec::new() };
                RemoteTree { remote, key, open, branches, short, rows }
            })
            .collect();
        let order: Vec<String> = trees.iter().flat_map(|tree| leaves(&tree.rows, &tree.branches)).collect();
        for tree in trees.iter().take(visible(self.layout.sections[1])) {
            let remote = tree.remote;
            let count = tree.branches.len().to_string();
            let resp = fold_row(ui, c, 0, ic::CLOUD, c.on_surface_variant, &remote.name, true, tree.open, Trail::Text(if tree.open { "" } else { &count }));
            if resp.clicked() {
                toggle(&mut self.opened, &tree.key);
            }
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
            for row in &tree.rows {
                match row {
                    TreeRow::Leaf { index, name, depth } => self.remote_branch_row(ui, ctx, snap, c, tree.branches[*index], name, depth + 1, &order, &remote.name),
                    TreeRow::Folder { key, name, depth, open } => {
                        let members = folder_members(key, &tree.key, &tree.short).into_iter().map(|i| tree.branches[i].name.clone()).collect();
                        self.folder_row(ui, c, key, name, depth + 1, *open, Group::Remote, members);
                    }
                }
            }
        }

        // ponytail: plain (non-virtualised) rows; cap the tag list, raise or virtualise if repos need more.
        let tags: Vec<&git::Tag> = snap.tags.iter().filter(|t| shown(&t.name)).take(300).collect();
        w::section_header(ui, "Tags", snap.tags.len(), Some(&mut self.layout.sections[2]), None);
        let order: Vec<String> = tags.iter().map(|t| t.name.clone()).collect();
        for tag in tags.iter().take(visible(self.layout.sections[2])) {
            let marked = self.is_marked(Group::Tag, &tag.name);
            let resp = side_row(ui, c, 0, ic::TAG, c.on_surface_variant, &tag.name, false, marked, Trail::Text(""));
            if resp.clicked() && !self.mark(ui, Group::Tag, &order, &tag.name) {
                self.jump_to(ctx, snap, &tag.sha);
            }
            resp.context_menu(|ui| match self.in_batch(Group::Tag, &tag.name) {
                true => self.batch_menu(ui, ctx, snap),
                false => self.tag_menu(ui, ctx, snap, tag),
            });
        }

        if !snap.submodules.is_empty() {
            if w::section_header(ui, "Submodules", snap.submodules.len(), Some(&mut self.layout.sections[3]), Some((ic::REFRESH, "Update submodules"))) {
                self.run(ctx, vec![Op::SubmoduleUpdate]);
            }
            for module in snap.submodules.iter().take(visible(self.layout.sections[3])) {
                let resp = side_row(ui, c, 0, ic::SUBMODULE, c.on_surface_variant, module, false, false, Trail::Text(""));
                if resp.double_clicked() {
                    self.open_request = Some(self.path.join(module));
                }
                resp.on_hover_text("Double-click to open in a new tab");
            }
        }

        if w::section_header(ui, "Worktrees", snap.worktrees.len(), Some(&mut self.layout.sections[4]), Some((ic::ADD, "Add worktree"))) {
            self.dialog = Some(dialogs::add_worktree(&self.path, None));
        }
        let order: Vec<String> = snap.worktrees.iter().map(|t| t.path.clone()).collect();
        for tree in snap.worktrees.iter().take(visible(self.layout.sections[4])) {
            let (_, name) = split_path(&tree.path);
            let here = Path::new(&tree.path) == self.path;
            let (trail, tip) = if tree.prunable {
                (Trail::Icon(ic::WARNING, c.error), format!("{}\nFolder is missing. Prune to drop this entry.", tree.path))
            } else {
                (Trail::Text(&tree.branch), tree.path.clone())
            };
            let tint = if here { c.primary } else { c.on_surface_variant };
            let marked = self.is_marked(Group::Worktree, &tree.path);
            let resp = side_row(ui, c, 0, ic::WORKTREE, tint, name, here, marked, trail);
            let marking = resp.clicked() && self.mark(ui, Group::Worktree, &order, &tree.path);
            if !marking && resp.double_clicked() && !here && !tree.prunable {
                self.open_request = Some(PathBuf::from(&tree.path));
            }
            resp.on_hover_text(tip).context_menu(|ui| match self.in_batch(Group::Worktree, &tree.path) {
                true => self.batch_menu(ui, ctx, snap),
                false => self.worktree_menu(ui, ctx, tree, here),
            });
        }
    }

    // ------------------------------------------------------------ multi-select

    fn is_marked(&self, group: Group, key: &str) -> bool {
        self.mark_group == group && self.marked.contains(key)
    }

    /// The row is part of a selection of several: its context menu acts on all of them.
    fn in_batch(&self, group: Group, key: &str) -> bool {
        self.marked.len() > 1 && self.is_marked(group, key)
    }

    /// Handles a click on a sidebar row. Ctrl toggles the row, Shift extends from the
    /// last toggled row. Returns true when the click only changed the selection.
    fn mark(&mut self, ui: &Ui, group: Group, order: &[String], key: &str) -> bool {
        let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        if !ctrl && !shift {
            self.marked.clear();
            return false;
        }
        if self.mark_group != group {
            // One kind at a time, so a batch action always has one meaning.
            self.marked.clear();
            self.anchor = None;
            self.mark_group = group;
        }
        apply_selection(&mut self.marked, &mut self.anchor, order, key, shift);
        true
    }

    fn batch_words(&self) -> (&'static str, &'static str) {
        match self.mark_group {
            Group::Local => ("Delete", "branches"),
            Group::Remote => ("Delete", "remote branches"),
            Group::Tag => ("Delete", "tags"),
            Group::Worktree => ("Remove", "worktrees"),
        }
    }

    fn batch_menu(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot) {
        let (verb, noun) = self.batch_words();
        if w::menu_danger(ui, ic::DELETE, &format!("{verb} {} {noun}\u{2026}", self.marked.len())) {
            self.delete_marked(ctx, snap);
        }
        if w::menu_item(ui, ic::COPY, "Copy names") {
            ctx.copy_text(self.marked.iter().cloned().collect::<Vec<_>>().join("\n"));
        }
        if w::menu_item(ui, ic::CLOSE, "Clear selection") {
            self.marked.clear();
        }
    }

    fn selection_bar(&mut self, ui: &mut Ui, rect: Rect, ctx: &Context, snap: &Snapshot) {
        let c = w::colors(ui);
        w::card(ui, rect, c.secondary_container, rect.height() / 2.0);
        let (verb, noun) = self.batch_words();
        w::hchild(ui, "selection-bar", rect.shrink2(vec2(6.0, 0.0)), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if w::icon_btn(ic::CLOSE, "Clear selection (Esc)").size(36.0).show(ui).clicked() {
                self.marked.clear();
            }
            let count = format!("{} {noun}", self.marked.len());
            ui.label(egui::RichText::new(count).font(theme::emph(13.5)).color(c.on_secondary_container));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if w::btn(verb).icon(ic::DELETE).kind(Kind::Danger).height(36.0).show(ui).clicked() {
                    self.delete_marked(ctx, snap);
                }
            });
        });
    }

    /// Asks to delete everything marked. Items that can never go (the current branch,
    /// the main or current worktree) are left out, and the dialog says so.
    fn delete_marked(&mut self, ctx: &Context, snap: &Snapshot) {
        let group = self.mark_group;
        let fixed = |key: &String| match group {
            Group::Local => snap.branches.iter().any(|b| b.remote.is_none() && b.current && &b.name == key),
            Group::Worktree => snap.worktrees.iter().any(|t| &t.path == key && (t.main || Path::new(&t.path) == self.path)),
            Group::Remote | Group::Tag => false,
        };
        let names: Vec<String> = self.marked.iter().filter(|key| !fixed(key)).cloned().collect();
        let skipped = self.marked.len() - names.len();
        if names.is_empty() {
            self.notify("Nothing in the selection can be removed: it is checked out here".into(), true, ctx);
            return;
        }
        let (verb, noun) = self.batch_words();
        let mut shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
        if names.len() > shown.len() {
            shown.push("\u{2026}");
        }
        let mut body = format!("{verb} {} {noun}?\n\n{}", names.len(), shown.join("\n"));
        body += match group {
            Group::Remote => "\n\nEveryone using the remote loses them.",
            Group::Worktree => "\n\nThe folders are deleted. Branches and commits stay.",
            Group::Local | Group::Tag => "",
        };
        if skipped > 0 {
            body += "\n\nLeft out: what is checked out in this window.";
        }
        let (title, force) = match group {
            Group::Local => ("Delete branches", Some("Force: delete even if not merged")),
            Group::Remote => ("Delete remote branches", None),
            Group::Tag => ("Delete tags", None),
            Group::Worktree => ("Remove worktrees", Some("Force: also discard uncommitted changes there")),
        };
        let remotes: Vec<String> = snap.remotes.iter().map(|r| r.name.clone()).collect();
        self.dialog = Some(dialogs::batch(title, body, force, verb, move |force| {
            names.iter().filter_map(|name| batch_op(group, name, &remotes, force)).collect()
        }));
    }

    /// A foldable folder of branches. A click folds it; its menu selects what is inside.
    #[allow(clippy::too_many_arguments)]
    fn folder_row(&mut self, ui: &mut Ui, c: &Colors, key: &str, name: &str, depth: usize, open: bool, group: Group, members: Vec<String>) {
        let count = members.len().to_string();
        let icon = if open { ic::FOLDER_OPEN } else { ic::FOLDER };
        // The folder as branches name it: without `local/`, or `remote/` and the remote's name.
        let folder = match group {
            Group::Remote => key.splitn(3, '/').nth(2),
            _ => key.split_once('/').map(|(_, rest)| rest),
        };
        let folder = folder.unwrap_or(key).to_string();
        let tint = self.accent(c, &format!("{folder}/")).unwrap_or(c.on_surface_variant);
        let resp = fold_row(ui, c, depth, icon, tint, name, false, open, Trail::Text(if open { "" } else { &count }));
        if resp.clicked() {
            toggle(&mut self.opened, key);
        }
        // Under the pointer the row offers its colors, and keeps the button while they are open.
        let colors = resp.id.with("colors");
        let choosing = egui::Popup::is_id_open(ui.ctx(), colors);
        if choosing || ui.rect_contains_pointer(resp.rect) {
            let button = Rect::from_center_size(pos2(resp.rect.right() - 20.0, resp.rect.center().y), vec2(26.0, 26.0));
            let pressed = w::icon_btn(ic::PALETTE, if choosing { "" } else { "Folder color" }).size(26.0).kind(Kind::Tonal).show_at(ui, resp.id.with("color"), button);
            egui::Popup::menu(&pressed).id(colors).show(|ui| self.color_menu(ui, &folder));
        }
        resp.context_menu(|ui| {
            self.color_menu(ui, &folder);
            w::menu_separator(ui);
            if w::menu_item(ui, ic::STAGE_ALL, &format!("Select all {} inside", members.len())) {
                (self.mark_group, self.anchor) = (group, None);
                self.marked = members.iter().cloned().collect();
            }
        });
    }

    /// The swatches that give a branch folder its color, for a menu. The swatch under
    /// the pointer is tried on at once, in the sidebar and on the history's labels.
    fn color_menu(&mut self, ui: &mut Ui, folder: &str) {
        let current = self.folder_colors.get(folder).copied();
        let (clicked, hovered) = w::hue_swatches(ui, current);
        if let Some(hue) = hovered {
            self.color_preview = Some((folder.to_string(), hue));
            ui.ctx().request_repaint();
        }
        if let Some(hue) = clicked {
            self.folder_colors.insert(folder.to_string(), hue);
            ui.close();
        }
        if current.is_some() && w::menu_item(ui, ic::CANCEL, "No color") {
            self.folder_colors.remove(folder);
        }
    }

    /// The accent of the colored folder that `branch`, named without its remote, is in.
    fn accent(&self, c: &Colors, branch: &str) -> Option<Color32> {
        folder_hue(&self.tints, branch).map(|hue| c.tint(hue).0)
    }

    #[allow(clippy::too_many_arguments)]
    fn local_branch_row(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot, c: &Colors, branch: &Branch, label: &str, depth: usize, order: &[String]) {
        let track = match (branch.ahead, branch.behind) {
            (0, 0) => String::new(),
            (a, 0) => format!("\u{2191}{a}"),
            (0, b) => format!("\u{2193}{b}"),
            (a, b) => format!("\u{2191}{a} \u{2193}{b}"),
        };
        let trail = if branch.gone { Trail::Icon(ic::UPSTREAM_GONE, c.error) } else { Trail::Text(&track) };
        let (icon, tint) = if branch.current { (ic::CHECK_CIRCLE, c.primary) } else { (ic::BRANCH, self.accent(c, &branch.name).unwrap_or(c.on_surface_variant)) };
        let marked = self.is_marked(Group::Local, &branch.name);
        let resp = side_row(ui, c, depth, icon, tint, label, branch.current, marked, trail);
        // Inside a folder the row shows only the last part of the name.
        let resp = match (branch.gone, depth > 0) {
            (true, _) => resp.on_hover_text(format!("{}\nUpstream branch no longer exists on the remote", branch.name)),
            (false, true) => resp.on_hover_text(&branch.name),
            (false, false) => resp,
        };
        let marking = resp.clicked() && self.mark(ui, Group::Local, order, &branch.name);
        if !marking && resp.double_clicked() && !branch.current {
            self.run(ctx, vec![Op::Checkout { target: branch.name.clone() }]);
        } else if !marking && resp.clicked() {
            self.jump_to(ctx, snap, &branch.sha);
        }
        resp.context_menu(|ui| match self.in_batch(Group::Local, &branch.name) {
            true => self.batch_menu(ui, ctx, snap),
            false => self.local_branch_menu(ui, ctx, snap, branch),
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn remote_branch_row(&mut self, ui: &mut Ui, ctx: &Context, snap: &Snapshot, c: &Colors, branch: &Branch, label: &str, depth: usize, order: &[String], remote: &str) {
        let marked = self.is_marked(Group::Remote, &branch.name);
        let tint = self.accent(c, branch.name.get(remote.len() + 1..).unwrap_or("")).unwrap_or(c.on_surface_variant);
        let resp = side_row(ui, c, depth, ic::BRANCH, tint, label, false, marked, Trail::Text("")).on_hover_text(&branch.name);
        let marking = resp.clicked() && self.mark(ui, Group::Remote, order, &branch.name);
        if !marking && resp.double_clicked() {
            self.dialog = Some(dialogs::checkout_remote(branch.name.clone(), remote));
        } else if !marking && resp.clicked() {
            self.jump_to(ctx, snap, &branch.sha);
        }
        resp.context_menu(|ui| match self.in_batch(Group::Remote, &branch.name) {
            true => self.batch_menu(ui, ctx, snap),
            false => self.remote_branch_menu(ui, ctx, snap, branch, remote),
        });
    }

    fn worktree_menu(&mut self, ui: &mut Ui, ctx: &Context, tree: &git::Worktree, here: bool) {
        if !here && !tree.prunable && w::menu_item(ui, ic::FOLDER_OPEN, "Open in new tab") {
            self.open_request = Some(PathBuf::from(&tree.path));
        }
        if w::menu_item(ui, ic::COPY, "Copy path") {
            ctx.copy_text(tree.path.clone());
        }
        if w::menu_item(ui, ic::REFRESH, "Prune stale worktrees") {
            self.run(ctx, vec![Op::WorktreePrune]);
        }
        // The main work tree and the one this tab shows cannot be removed from here.
        if !tree.main && !here {
            w::menu_separator(ui);
            if w::menu_danger(ui, ic::DELETE, "Remove worktree\u{2026}") {
                self.dialog = Some(dialogs::remove_worktree(tree));
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
            if w::menu_item(ui, ic::WORKTREE, "Check out in a new worktree\u{2026}") {
                self.dialog = Some(dialogs::add_worktree(&self.path, Some(&name)));
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

    /// The list card, and the area for the file list and diff cards under it. The list
    /// has the whole height until something is picked; then the details slide up.
    fn list_and_detail(&mut self, ui: &mut Ui, rect: Rect) -> (Rect, Option<Rect>) {
        let picked = self.detail.is_some() || self.wanted.is_some();
        let shown = ui.ctx().animate_bool_with_time(self.id("detail-shown"), picked, 0.18);
        if shown == 0.0 {
            return (rect, None);
        }
        let split_h = (rect.height() * self.layout.split).clamp(120.0, (rect.height() - 200.0).max(120.0));
        let list = Rect::from_min_size(rect.min, vec2(rect.width(), egui::lerp(rect.height()..=split_h, shown)));
        let gap = Rect::from_min_size(pos2(rect.left(), list.bottom()), vec2(rect.width(), GAP));
        self.layout.split += w::splitter(ui, self.id("list-split"), gap, false) / rect.height().max(1.0);
        // Full size from the start, so the cards slide rather than stretch.
        let detail = Rect::from_min_size(pos2(rect.left(), gap.bottom()), vec2(rect.width(), rect.height() - split_h - GAP));
        (list, Some(detail))
    }

    fn history_page(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let (list, detail) = self.list_and_detail(ui, rect);
        self.history_list(ui, list, snap);
        if let Some(detail) = detail {
            self.detail_cards(ui, detail);
        }
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
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let one_remote = snap.remotes.len() == 1;
        w::rows(ui, "history-rows", body, ROW, snap.commits.len(), scroll_to, |ui, range| {
            let origin = pos2(ui.max_rect().left() + 4.0, ui.max_rect().top() - range.start as f32 * ROW);
            for i in range.clone() {
                let commit = &snap.commits[i];
                let (row, resp) = w::row(ui, ROW);
                let selected = self.selected.as_deref() == Some(commit.sha.as_str());
                w::row_bg(ui, row, &resp, selected);
                // A second click on the picked commit closes its details.
                match (resp.clicked(), selected) {
                    (true, true) => self.deselect(),
                    (true, false) => self.select_commit(&ctx, commit),
                    _ => {}
                }
                resp.context_menu(|ui| self.commit_menu(ui, &ctx, snap, commit));
                let margin = graph.margins.get(i).copied().unwrap_or(10.0);
                let tip = paint_commit_row(ui.painter(), &c, row, commit, margin, selected, now, one_remote, &self.tints, resp.hover_pos());
                if let Some(tip) = tip {
                    resp.on_hover_text(tip);
                }
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
        self.layout.files = self.layout.files.clamp(220.0, (rect.width() - 280.0).max(220.0));
        let left = Rect::from_min_size(rect.min, vec2(self.layout.files, rect.height()));
        let gap = Rect::from_min_size(pos2(left.right(), rect.top()), vec2(GAP, rect.height()));
        self.layout.files += w::splitter(ui, self.id(salt), gap, true);
        (left, Rect::from_min_max(pos2(gap.right(), rect.top()), rect.max))
    }

    fn detail_cards(&mut self, ui: &mut Ui, rect: Rect) {
        let c = w::colors(ui);
        let Some(detail) = self.detail.clone() else {
            // Loading, or sliding away after being closed.
            w::card(ui, rect, c.surface, CARD);
            if self.wanted.is_some() {
                w::loading_indicator(ui, rect.center(), 22.0);
            }
            return;
        };
        let (left, right) = self.split_files(ui, "detail-split", rect);
        self.files_card(ui, left, &detail);
        self.diff_card(ui, right);
        let close = Rect::from_min_size(pos2(left.right() - 40.0, left.top() + 10.0), vec2(30.0, 30.0));
        if w::icon_btn(ic::CLOSE, "Close details (Esc)").size(30.0).show_at(ui, self.id("close-detail"), close).clicked() {
            self.deselect();
        }
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
            let text = Rect::from_min_max(pos2(inner.left() + 48.0, y + 4.0), pos2(inner.right() - 34.0, y + 24.0));
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
            let text = Rect::from_min_max(pos2(inner.left() + 48.0, y + 4.0), pos2(inner.right() - 34.0, y + 24.0));
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
        let rows = self.file_rows.take().unwrap_or_else(|| file_rows(&detail.files, "files", &self.folded));
        let mut refold = false;
        w::rows(ui, "detail-files", list, ROW, rows.len(), None, |ui, range| {
            for row in &rows[range] {
                let (i, depth) = match row {
                    FileRow::Folder { key, name, depth, open } => {
                        let icon = if *open { ic::FOLDER_OPEN } else { ic::FOLDER };
                        if fold_row(ui, &c, *depth, icon, c.on_surface_variant, name, false, *open, Trail::Text("")).clicked() {
                            toggle(&mut self.folded, key);
                            refold = true;
                        }
                        continue;
                    }
                    FileRow::File { index, depth } => (*index, *depth),
                };
                let change = &detail.files[i];
                let (row, resp) = w::row(ui, ROW);
                let selected = self.detail_file == Some(i);
                w::row_bg(ui, row, &resp, selected);
                paint_tree_file(ui.painter(), &c, row, change, change.index, depth, selected);
                let resp = resp.on_hover_text(&change.path);
                if resp.clicked() {
                    self.pick_file(&ctx, i, change.clone());
                }
                resp.context_menu(|ui| {
                    if self.can_diff_externally(change) && w::menu_item(ui, ic::COMPARE, "Open in diff tool") {
                        self.open_in_diff_tool(&ctx, DiffSpec::Revs { base: detail.base.clone(), rev: detail.sha.clone(), change: change.clone() });
                    }
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
        // Picking a file loads new details only on another commit, so the rows still fit.
        self.file_rows = (!refold && self.detail.as_ref().is_some_and(|d| d.sha == detail.sha)).then_some(rows);
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
        x += w::label_in(&p, Rect::from_min_max(pos2(x, header.top()), header.max - vec2(190.0, 0.0)), name, theme::emph(14.0), c.on_surface) + 10.0;
        w::label_in(&p, Rect::from_min_max(pos2(x, header.top()), header.max - vec2(150.0, 0.0)), dir, theme::body(12.5), c.on_surface_variant);
        // The same diff in the program picked in Settings, when there is one.
        let external = self.diff_spec.clone().filter(|spec| match spec {
            DiffSpec::Unstaged(change) | DiffSpec::Staged(change) | DiffSpec::Revs { change, .. } => self.can_diff_externally(change) && !change.conflicted(),
        });
        let mut stats_right = header.right() - 10.0;
        if let Some(spec) = external {
            let button = Rect::from_center_size(pos2(header.right() - 18.0, header.center().y), vec2(30.0, 30.0));
            let tip = format!("Open in {}", self.diff_tool.as_ref().map_or("diff tool", |(name, _)| name));
            if w::icon_btn(ic::COMPARE, &tip).size(30.0).show_at(ui, self.id("diff-tool"), button).clicked() {
                let ctx = ui.ctx().clone();
                self.open_in_diff_tool(&ctx, spec);
            }
            stats_right = button.left() - 8.0;
        }
        let stats = p.text(pos2(stats_right, header.center().y), Align2::RIGHT_CENTER, format!("\u{2212}{}", diff.deleted), theme::emph(13.0), c.deleted);
        p.text(pos2(stats.left() - 8.0, header.center().y), Align2::RIGHT_CENTER, format!("+{}", diff.added), theme::emph(13.0), c.added);

        let body = Rect::from_min_max(pos2(inner.left(), header.bottom() + 4.0), inner.max);
        if diff.lines.is_empty() {
            let (title, hint) = if diff.binary { ("Binary file", "No text preview for this change") } else { ("No text changes", "Only metadata changed (mode, rename)") };
            w::empty_state(ui, body, ic::FILE, title, hint);
            return;
        }
        // A change's lines can be staged or unstaged on their own, picked here.
        let reverse = match &self.diff_spec {
            Some(DiffSpec::Unstaged(change)) if !change.conflicted() => Some(false),
            Some(DiffSpec::Staged(_)) => Some(true),
            _ => None,
        };
        let reverse = reverse.filter(|_| diff.patchable());
        let mut pick = std::mem::take(&mut self.line_pick);
        let wanted = w::child(ui, "diff-body", body, |ui| paint_diff(ui, &c, &diff, reverse.map(|reverse| (&mut pick, reverse))));
        self.line_pick = pick;
        if let (Some(lines), Some(reverse)) = (wanted, reverse)
            && let Some(patch) = diff.patch(|i| lines.contains(&i), reverse)
        {
            let ctx = ui.ctx().clone();
            self.stage_lines(&ctx, patch, reverse);
        }
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
        let conflicted = self.picked.as_ref().and_then(|(path, staged)| snap.changes.iter().find(|ch| !staged && ch.conflicted() && &ch.path == path)).cloned();
        match conflicted {
            Some(change) => self.conflict_panel(ui, diff, snap, &change),
            None => self.diff_card(ui, diff),
        }

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

    /// Shown instead of a diff for a conflicted file: both sides with the commits that
    /// touched the file, a checkbox each, and one button whose meaning follows the
    /// ticks and what kind of conflict it is.
    fn conflict_panel(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot, change: &Change) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        w::card(ui, rect, c.surface, CARD);
        let p = ui.painter().with_clip_rect(rect);
        let cx = rect.center().x;
        let top = rect.top() + (rect.height() * 0.05).clamp(16.0, 40.0);
        let operation = snap.state.label();
        let heading = if operation.is_empty() { "Conflict".to_string() } else { format!("{operation} conflict") };
        let title = p.text(pos2(cx + 14.0, top + 12.0), Align2::CENTER_CENTER, heading, theme::emph(20.0), c.on_surface);
        w::icon_at(&p, pos2(title.left() - 20.0, top + 12.0), ic::WARNING, 24.0, c.tertiary);
        p.text(pos2(cx, top + 40.0), Align2::CENTER_CENTER, "Select the changes or merge them manually", theme::body(13.5), c.on_surface_variant);
        let file = p.text(pos2(cx + 10.0, top + 70.0), Align2::CENTER_CENTER, &change.path, theme::body(14.5), c.on_surface);
        w::icon_at(&p, pos2(file.left() - 14.0, top + 70.0), ic::FILE, 16.0, c.on_surface_variant);
        let kind = format!("{}{}  {}", change.index, change.work, change.conflict_reason());
        let kind_width = w::text_width(&p, &kind, theme::emph(11.5)) + 30.0;
        w::chip(&p, pos2(cx - kind_width / 2.0, top + 98.0), ic::WARNING, &kind, c.error_container, c.on_error_container);

        let info = self.sides.as_ref().filter(|(path, _)| path == &change.path).and_then(|(_, info)| info.as_ref());
        let gap = 132.0;
        let width = ((rect.width() - gap - 48.0) / 2.0).clamp(180.0, 330.0);
        let cards_top = top + 126.0;
        // Tall enough for the longer commit list, within what the card leaves free.
        let commits = info.map_or(1, |i| i.current.commits.len().max(i.incoming.commits.len()).max(1));
        let height = (64.0 + commits as f32 * 22.0).min((rect.bottom() - cards_top - 118.0).clamp(90.0, 236.0));
        let left = Rect::from_min_size(pos2(cx - gap / 2.0 - width, cards_top), vec2(width, height));
        let right = Rect::from_min_size(pos2(cx + gap / 2.0, cards_top), vec2(width, height));
        // What each side did to the file: a 'U' next to an 'A' means that side lacks it.
        let status = |own: char, other: char| match (own, other) {
            ('A', _) => ('A', "added"),
            ('D', _) => ('D', "deleted"),
            ('U', 'A') => ('D', "not present"),
            _ => ('M', "modified"),
        };
        let names = (info.map_or_else(|| snap.head.label(), |i| i.current.name.clone()), info.map_or_else(|| "incoming".to_string(), |i| i.incoming.name.clone()));
        side_card(ui, &c, left, info.map(|i| &i.current), &names.0, c.primary, self.take.0, status(change.index, change.work));
        side_card(ui, &c, right, info.map(|i| &i.incoming), &names.1, c.tertiary, self.take.1, status(change.work, change.index));

        // Checkboxes between the cards, joined to each other and to the button below.
        let y = cards_top + 24.0;
        let (box_left, box_right) = (pos2(left.right() + 24.0, y), pos2(right.left() - 24.0, y));
        let button_y = cards_top + height + 40.0;
        let line = |on: bool, color| Stroke::new(1.5, if on { color } else { c.outline_variant });
        p.line_segment([box_left + vec2(16.0, 0.0), pos2(cx, y)], line(self.take.0, c.primary));
        p.line_segment([pos2(cx, y), box_right - vec2(16.0, 0.0)], line(self.take.1, c.tertiary));
        p.line_segment([pos2(cx, y), pos2(cx, button_y - 26.0)], line(self.take.0 || self.take.1, c.on_surface_variant));
        let check = |on: bool| if on { w::Check::On } else { w::Check::Off };
        if w::checkbox_at(ui, self.id("take-current"), box_left, check(self.take.0), c.primary, c.on_primary).on_hover_text("Take this side").clicked() {
            self.take.0 = !self.take.0;
        }
        if w::checkbox_at(ui, self.id("take-incoming"), box_right, check(self.take.1), c.tertiary, c.on_tertiary).on_hover_text("Take this side").clicked() {
            self.take.1 = !self.take.1;
        }

        // One button: merge both sides, take one, or stage a file fixed elsewhere.
        enum Choice {
            Nothing,
            Editor,
            Run(Op),
        }
        let one_side = |theirs: bool, name: &str| {
            let op = Choice::Run(Op::Resolve { change: change.clone(), theirs });
            match change.resolution_deletes(theirs) {
                true => (format!("Delete, as on {name}"), ic::DELETE, op, String::new()),
                false => (format!("Use {name}"), ic::CHECK, op, String::new()),
            }
        };
        let merge = |choice: Choice, note: String| ("Merge".to_string(), ic::MERGE, choice, note);
        let (label, icon, choice, note) = match (self.take, info.map(|i| i.state)) {
            ((true, false), _) => one_side(false, &names.0),
            ((false, true), _) => one_side(true, &names.1),
            ((false, false), _) => merge(Choice::Nothing, "Tick both sides to merge them, or one side to take it whole.".into()),
            ((true, true), None) => merge(Choice::Nothing, String::new()),
            ((true, true), Some(FileState::Text)) => merge(Choice::Editor, String::new()),
            ((true, true), Some(FileState::Resolved)) => {
                let stage = Choice::Run(Op::Stage { paths: vec![change.path.clone()] });
                ("Mark resolved".to_string(), ic::CHECK, stage, "The conflict markers are gone from this file. Staging it marks it resolved.".to_string())
            }
            ((true, true), Some(FileState::Binary)) => merge(Choice::Nothing, "Binary or very large file: it cannot be merged line by line. Tick one side.".into()),
            ((true, true), Some(FileState::OneSided)) => merge(Choice::Nothing, format!("This file is {}: there is nothing to merge. Tick one side.", change.conflict_reason())),
        };
        let area = Rect::from_center_size(pos2(cx, button_y), vec2(rect.width() - 32.0, 44.0));
        let mut actions = ui.new_child(egui::UiBuilder::new().id_salt("conflict-actions").max_rect(area).layout(Layout::top_down(Align::Center)));
        let enabled = !matches!(choice, Choice::Nothing) && self.busy.is_none();
        let clicked = w::btn(&label).icon(icon).height(44.0).enabled(enabled).show(&mut actions).clicked();
        p.text(pos2(cx, button_y + 38.0), Align2::CENTER_CENTER, note, theme::body(12.5), c.on_surface_variant);
        match (clicked, choice) {
            (true, Choice::Run(op)) => self.run(&ctx, vec![op]),
            (true, Choice::Editor) => self.open_conflict(&ctx, change),
            _ => {}
        }
    }

    fn progress_banner(&mut self, ui: &mut Ui, rect: Rect, snap: &Snapshot) {
        let c = w::colors(ui);
        let ctx = ui.ctx().clone();
        // A quiet tint: the bright container color would wash out the buttons on it.
        w::card(ui, rect, Colors::state(c.surface_high, c.tertiary, 0.22), shape::LARGE);
        let p = ui.painter();
        w::icon_at(p, pos2(rect.left() + 22.0, rect.top() + 22.0), ic::WARNING, 20.0, c.tertiary);
        let conflicts = snap.changes.iter().filter(|ch| ch.conflicted()).count();
        let text = if conflicts > 0 { format!("{} in progress \u{00b7} {conflicts} conflict(s)", snap.state.label()) } else { format!("{} in progress", snap.state.label()) };
        p.text(pos2(rect.left() + 40.0, rect.top() + 22.0), Align2::LEFT_CENTER, text, theme::emph(13.5), c.on_surface);
        let state = snap.state;
        let actions = Rect::from_min_max(pos2(rect.left() + 10.0, rect.top() + 42.0), rect.max - vec2(10.0, 8.0));
        w::hchild(ui, "progress-actions", actions, |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let first = snap.changes.iter().find(|ch| ch.conflicted());
            if let Some(change) = first
                && w::btn("Resolve").icon(ic::EDIT).height(32.0).show(ui).on_hover_text("Open the conflict editor").clicked()
            {
                self.open_conflict(&ctx, change);
            }
            let kind = if first.is_some() { Kind::Tonal } else { Kind::Filled };
            if w::btn("Continue").icon(ic::PLAY).kind(kind).height(32.0).enabled(conflicts == 0).show(ui).clicked() {
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
        let flip = |changes: &[&Change]| -> Op {
            let paths = changes.iter().flat_map(|ch| std::iter::once(&ch.path).chain(&ch.orig)).cloned().collect();
            if staged { Op::Unstage { paths } } else { Op::Stage { paths } }
        };
        let (flip_icon, flip_tip, flip_folder) = if staged { (ic::REMOVE, "Unstage", "Unstage folder") } else { (ic::ADD, "Stage", "Stage folder") };
        // The button at the right end of the row under the pointer.
        let flip_btn = |ui: &Ui, resp: &Response, tip: &str| -> bool {
            let button = Rect::from_center_size(pos2(resp.rect.right() - 20.0, resp.rect.center().y), vec2(26.0, 26.0));
            ui.rect_contains_pointer(resp.rect) && w::icon_btn(flip_icon, tip).size(26.0).kind(Kind::Tonal).show_at(ui, resp.id.with("toggle"), button).clicked()
        };
        let list = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        let scope = if staged { "staged" } else { "unstaged" };
        let paths: Vec<&str> = changes.iter().map(|ch| ch.path.as_str()).collect();
        // ponytail: tree rebuilt every frame, cache it like `file_rows` if huge change sets get slow.
        let rows = file_rows(changes.iter().copied(), scope, &self.change_folds);
        w::rows(ui, if staged { "staged-rows" } else { "unstaged-rows" }, list, ROW, rows.len(), None, |ui, range| {
            for row in &rows[range] {
                let (i, depth) = match row {
                    FileRow::Folder { key, name, depth, open } => {
                        let icon = if *open { ic::FOLDER_OPEN } else { ic::FOLDER };
                        let resp = fold_row(ui, &c, *depth, icon, c.on_surface_variant, name, false, *open, Trail::Text(""));
                        // A double click folds and unfolds again, so the folder ends as it was.
                        if resp.clicked() {
                            toggle(&mut self.change_folds, key);
                        }
                        if resp.double_clicked() || flip_btn(ui, &resp, flip_folder) {
                            // Conflicted files stay: staging one would mark it resolved unseen.
                            let inside: Vec<&Change> = folder_members(key, scope, &paths).into_iter().map(|i| changes[i]).filter(|ch| !ch.conflicted()).collect();
                            if !inside.is_empty() {
                                self.run(&ctx, vec![flip(&inside)]);
                            }
                        }
                        continue;
                    }
                    FileRow::File { index, depth } => (*index, *depth),
                };
                let change = changes[i];
                let (row, resp) = w::row(ui, ROW);
                let resp = resp.on_hover_text(&change.path);
                let selected = self.picked.as_ref().is_some_and(|(path, s)| *s == staged && path == &change.path);
                w::row_bg(ui, row, &resp, selected);
                let status = match (staged, change.conflicted(), change.untracked()) {
                    (true, ..) => change.index,
                    (_, true, _) => 'U',
                    (_, _, true) => '?',
                    _ => change.work,
                };
                let hot = ui.rect_contains_pointer(row);
                // The stage button takes the right end of the row under the pointer.
                paint_tree_file(ui.painter(), &c, if hot { row.with_max_x(row.right() - 34.0) } else { row }, change, status, depth, selected);
                if resp.double_clicked() && change.conflicted() {
                    self.open_conflict(&ctx, change);
                } else if resp.double_clicked() {
                    self.run(&ctx, vec![flip(&[change])]);
                } else if resp.clicked() {
                    self.pick_change(&ctx, change, staged);
                }
                if flip_btn(ui, &resp, flip_tip) {
                    self.run(&ctx, vec![flip(&[change])]);
                }
                resp.context_menu(|ui| {
                    if w::menu_item(ui, flip_icon, flip_tip) {
                        self.run(&ctx, vec![flip(&[change])]);
                    }
                    if change.conflicted() {
                        if w::menu_item(ui, ic::EDIT, "Resolve in editor\u{2026}") {
                            self.open_conflict(&ctx, change);
                        }
                        for (label, theirs) in [("Resolve using ours", false), ("Resolve using theirs", true)] {
                            if w::menu_item(ui, ic::MERGE, label) {
                                self.run(&ctx, vec![Op::Resolve { change: change.clone(), theirs }]);
                            }
                        }
                    }
                    w::menu_separator(ui);
                    if self.can_diff_externally(change) && !change.conflicted() && w::menu_item(ui, ic::COMPARE, "Open in diff tool") {
                        self.open_in_diff_tool(&ctx, if staged { DiffSpec::Staged(change.clone()) } else { DiffSpec::Unstaged(change.clone()) });
                    }
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
        if let Some(detail) = detail {
            self.detail_cards(ui, detail);
        }
    }

    // ------------------------------------------------------------ overlays

    fn overlays(&mut self, ctx: &Context) {
        if let Some(dialog) = &mut self.dialog {
            match dialog.show(ctx) {
                Outcome::Open => {}
                Outcome::Cancelled => self.dialog = None,
                Outcome::Confirmed(ops, keep_going) => {
                    self.dialog = None;
                    self.start(ctx, ops, keep_going);
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
        self.snackbar(ctx);
    }

    fn snackbar(&mut self, ctx: &Context) {
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

/// One visible row of a branch tree.
#[derive(Debug, PartialEq)]
pub(crate) enum TreeRow<'a> {
    /// `key` identifies the folder for folding: the scope, then its path.
    Folder { key: String, name: &'a str, depth: usize, open: bool },
    /// `index` points into the names the tree was built from; `name` is the last path part.
    Leaf { index: usize, name: &'a str, depth: usize },
}

/// Lays names (branches, file paths) out as a tree by their `/` separated parts: at
/// each level the folders come first, then the leaves, both alphabetical. A folder
/// that `is_open` rejects for its key is listed closed, without its contents.
pub(crate) fn tree_rows<'a>(names: &[&'a str], scope: &str, is_open: &dyn Fn(&str) -> bool) -> Vec<TreeRow<'a>> {
    fn level<'a>(items: &[(usize, &'a str)], depth: usize, path: &str, is_open: &dyn Fn(&str) -> bool, rows: &mut Vec<TreeRow<'a>>) {
        let mut folders: std::collections::BTreeMap<(String, &'a str), Vec<(usize, &'a str)>> = Default::default();
        let mut leaves: Vec<(usize, &'a str)> = Vec::new();
        for &(index, rest) in items {
            match rest.split_once('/') {
                Some((folder, inside)) if !folder.is_empty() && !inside.is_empty() => folders.entry((folder.to_lowercase(), folder)).or_default().push((index, inside)),
                _ => leaves.push((index, rest)),
            }
        }
        for ((_, name), inside) in folders {
            let key = format!("{path}/{name}");
            let open = is_open(&key);
            rows.push(TreeRow::Folder { key: key.clone(), name, depth, open });
            if open {
                level(&inside, depth + 1, &key, is_open, rows);
            }
        }
        leaves.sort_by_key(|(_, name)| name.to_lowercase());
        rows.extend(leaves.into_iter().map(|(index, name)| TreeRow::Leaf { index, name, depth }));
    }
    let items: Vec<(usize, &str)> = names.iter().copied().enumerate().collect();
    let mut rows = Vec::new();
    level(&items, 0, scope, is_open, &mut rows);
    rows
}

/// The hue of the folder `branch` is in: that of its nearest colored folder.
pub(crate) fn folder_hue(colors: &BTreeMap<String, f32>, branch: &str) -> Option<f32> {
    let mut folder = branch;
    while let Some((parent, _)) = folder.rsplit_once('/') {
        if let Some(hue) = colors.get(parent) {
            return Some(*hue);
        }
        folder = parent;
    }
    None
}

/// Indices of the names that live under the folder `key` of the tree built for `scope`.
pub(crate) fn folder_members(key: &str, scope: &str, names: &[&str]) -> Vec<usize> {
    let prefix = format!("{}/", key.get(scope.len() + 1..).unwrap_or(""));
    names.iter().enumerate().filter(|(_, name)| name.starts_with(&prefix)).map(|(index, _)| index).collect()
}

fn toggle(set: &mut BTreeSet<String>, key: &str) {
    if !set.remove(key) {
        set.insert(key.to_string());
    }
}

/// A sidebar row that folds: the usual row plus a chevron in front of its icon.
#[allow(clippy::too_many_arguments)]
fn fold_row(ui: &mut Ui, c: &Colors, indent: usize, icon: &str, tint: Color32, text: &str, emphasized: bool, open: bool, trail: Trail) -> Response {
    let resp = side_row(ui, c, indent, icon, tint, text, emphasized, false, trail);
    let chevron = pos2(resp.rect.left() + 10.0 + indent as f32 * 18.0, resp.rect.center().y);
    w::icon_at(ui.painter(), chevron, if open { ic::EXPAND } else { ic::CHEVRON }, 16.0, c.on_surface_variant);
    resp
}

/// What sits at the right edge of a sidebar row.
#[derive(Clone, Copy)]
enum Trail<'a> {
    /// Quiet text such as ahead/behind counts; empty for nothing.
    Text(&'a str),
    /// A status icon in the given color.
    Icon(&'a str, Color32),
}

fn side_row(ui: &mut Ui, c: &Colors, indent: usize, icon: &str, tint: Color32, text: &str, emphasized: bool, marked: bool, trail: Trail) -> Response {
    let (row, resp) = w::row(ui, ROW);
    w::row_bg(ui, row, &resp, marked);
    let fg = if marked { c.on_secondary_container } else { c.on_surface };
    let p = ui.painter();
    let left = row.left() + 26.0 + indent as f32 * 18.0;
    w::icon_at(p, pos2(left, row.center().y), icon, 16.0, tint);
    let trail_w = match trail {
        Trail::Text("") => 0.0,
        Trail::Text(label) => w::text_width(p, label, theme::body(12.0)) + 10.0,
        Trail::Icon(..) => 26.0,
    };
    let font = if emphasized { theme::emph(13.5) } else { theme::body(13.5) };
    w::label_in(p, Rect::from_min_max(pos2(left + 14.0, row.top()), pos2(row.right() - 8.0 - trail_w, row.bottom())), text, font, fg);
    match trail {
        Trail::Text(label) => {
            p.text(pos2(row.right() - 10.0, row.center().y), Align2::RIGHT_CENTER, label, theme::body(12.0), c.on_surface_variant);
        }
        Trail::Icon(glyph, color) => w::icon_at(p, pos2(row.right() - 18.0, row.center().y), glyph, 16.0, color),
    }
    resp
}

/// One side of a conflict: branch, what it did to the file, and its commits on it.
#[allow(clippy::too_many_arguments)]
fn side_card(ui: &Ui, c: &Colors, rect: Rect, side: Option<&ConflictSide>, name: &str, accent: Color32, taken: bool, status: (char, &str)) {
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, w::radius(shape::LARGE), c.surface_low);
    let outline = if taken { Stroke::new(1.5, accent) } else { Stroke::new(1.0, c.outline_variant) };
    p.rect_stroke(rect, w::radius(shape::LARGE), outline, egui::StrokeKind::Inside);
    let mid = rect.top() + 24.0;
    w::icon_at(&p, pos2(rect.left() + 20.0, mid), ic::BRANCH, 18.0, accent);
    let status_w = w::text_width(&p, status.1, theme::body(12.0)) + 36.0;
    w::label_in(&p, Rect::from_min_max(pos2(rect.left() + 36.0, mid - 12.0), pos2(rect.right() - status_w - 8.0, mid + 12.0)), name, theme::emph(14.5), c.on_surface);
    w::status_badge(&p, pos2(rect.right() - status_w + 8.0, mid), status.0, c);
    p.text(pos2(rect.right() - 12.0, mid), Align2::RIGHT_CENTER, status.1, theme::body(12.0), c.on_surface_variant);
    p.hline(rect.shrink(14.0).x_range(), rect.top() + 46.0, Stroke::new(1.0, c.outline_variant));

    let list = Rect::from_min_max(pos2(rect.left() + 12.0, rect.top() + 54.0), rect.max - vec2(12.0, 10.0));
    let Some(side) = side else {
        w::loading_indicator(ui, list.center(), 14.0);
        return;
    };
    if side.commits.is_empty() {
        let empty = if side.tip.is_some() { "No commits touch this file here" } else { "Not a commit" };
        p.text(list.left_top() + vec2(4.0, 12.0), Align2::LEFT_CENTER, empty, theme::body(12.5), c.on_surface_variant);
    }
    let fits = (list.height() / 22.0).floor() as usize;
    for (index, commit) in side.commits.iter().take(fits).enumerate() {
        let row = Rect::from_min_size(pos2(list.left(), list.top() + index as f32 * 22.0), vec2(list.width(), 22.0));
        let more = index + 1 == fits && side.commits.len() > fits;
        if more {
            p.text(pos2(row.left() + 4.0, row.center().y), Align2::LEFT_CENTER, format!("and {} more", side.commits.len() - index), theme::body(12.0), c.on_surface_variant);
            break;
        }
        w::avatar(&p, pos2(row.left() + 10.0, row.center().y), 8.0, &commit.author, c);
        p.text(pos2(row.left() + 24.0, row.center().y), Align2::LEFT_CENTER, commit.sha.get(..7).unwrap_or(&commit.sha), theme::mono(11.5), c.on_surface_variant);
        let date = commit.date.get(..10).unwrap_or(&commit.date);
        let date_w = w::text_width(&p, date, theme::body(12.0));
        p.text(pos2(row.right() - 2.0, row.center().y), Align2::RIGHT_CENTER, date, theme::body(12.0), c.on_surface_variant);
        w::label_in(&p, Rect::from_min_max(pos2(row.left() + 82.0, row.top()), pos2(row.right() - date_w - 12.0, row.bottom())), &commit.subject, theme::body(12.5), c.on_surface);
    }
}

/// How long ago, in few characters: `now`, `26 min`, `3 h`, `5 d`, `7 wk`. Past a
/// year it is the day itself, taken from `date` (`2024-03-02 12:00`).
pub(crate) fn ago(time: i64, now: i64, date: &str) -> String {
    let secs = (now - time).max(0);
    match secs {
        0..60 => "now".into(),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h", secs / 3_600),
        86_400..1_209_600 => format!("{} d", secs / 86_400),
        1_209_600..31_536_000 => format!("{} wk", secs / 604_800),
        _ => date.get(..10).unwrap_or(date).to_string(),
    }
}

/// Keeps both ends of a long name: `fix/Bug-Hunt-Edit-Receipts` -> `fix/Bug-H…Edit-Receipts`.
pub(crate) fn clip_name(name: &str) -> std::borrow::Cow<'_, str> {
    const HEAD: usize = 9;
    const TAIL: usize = 13;
    let count = name.chars().count();
    if count <= HEAD + TAIL + 1 {
        return name.into();
    }
    let head: String = name.chars().take(HEAD).collect();
    let tail: String = name.chars().skip(count - TAIL).collect();
    format!("{head}\u{2026}{tail}").into()
}

/// The refs worth a chip, most telling first, and whether each local one has its
/// remote twin on the same commit. A twin gets no chip of its own.
pub(crate) fn ref_chips(refs: &[git::Ref]) -> Vec<(&git::Ref, bool)> {
    let local = |r: &git::Ref| matches!(r.kind, RefKind::Head | RefKind::Local);
    let twin_of = |r: &git::Ref| refs.iter().find(|l| local(l) && r.kind == RefKind::Remote && r.name.split_once('/').is_some_and(|(_, rest)| rest == l.name));
    let rank = |r: &git::Ref| match r.kind {
        RefKind::Head => 0,
        RefKind::Local => 1,
        RefKind::Tag => 2,
        RefKind::Remote => 3,
    };
    let mut chips: Vec<(&git::Ref, bool)> = refs.iter().filter(|r| twin_of(r).is_none()).map(|r| (r, refs.iter().any(|t| twin_of(t) == Some(r)))).collect();
    chips.sort_by_key(|(r, _)| rank(r));
    chips
}

/// A file in the tree: status badge and name, lined up with the folder rows. The
/// folders above say where it is, so only a rename's old path is added.
fn paint_tree_file(p: &Painter, c: &Colors, row: Rect, change: &Change, status: char, depth: usize, selected: bool) {
    let left = row.left() + 26.0 + depth as f32 * 18.0;
    w::status_badge(p, pos2(left, row.center().y), status, c);
    let (_, name) = split_path(&change.path);
    let fg = if selected { c.on_secondary_container } else { c.on_surface };
    let right = row.right() - 8.0;
    let mut x = left + 18.0;
    x += w::label_in(p, Rect::from_min_max(pos2(x, row.top()), pos2(right, row.bottom())), name, theme::body(13.0), fg) + 8.0;
    if let Some(orig) = &change.orig {
        w::label_in(p, Rect::from_min_max(pos2(x, row.top()), pos2(right, row.bottom())), &format!("\u{2190} {orig}"), theme::body(12.0), c.on_surface_variant);
    }
}

/// Paints one history row. The subject is the loud part; everything else is quiet.
/// Returns a tooltip for what the pointer is over: the full date, or all refs when
/// some are shortened or folded into `+2`.
#[allow(clippy::too_many_arguments)]
fn paint_commit_row(p: &Painter, c: &Colors, row: Rect, commit: &Commit, margin: f32, selected: bool, now: i64, one_remote: bool, hues: &BTreeMap<String, f32>, pointer: Option<Pos2>) -> Option<String> {
    const MAX_CHIPS: usize = 2;
    let fg = if selected { c.on_secondary_container } else { c.on_surface };
    let dim = if selected { c.on_secondary_container } else { c.on_surface_variant };
    let faint = if selected { c.on_secondary_container } else { c.outline };
    let mid = row.center().y;
    let date = Rect::from_min_max(pos2(row.right() - 84.0, row.top()), pos2(row.right() - 10.0, row.bottom()));
    let sha = Rect::from_min_size(pos2(date.left() - 62.0, row.top()), vec2(58.0, row.height()));
    let author_w = if row.width() > 760.0 { 140.0 } else { 0.0 };
    let author = Rect::from_min_size(pos2(sha.left() - author_w - 8.0, row.top()), vec2(author_w, row.height()));
    p.text(pos2(date.right(), mid), Align2::RIGHT_CENTER, ago(commit.time, now, &commit.date), theme::body(12.0), dim);
    // Seven characters, as git abbreviates; the details card has the longer form to copy.
    w::label_in(p, sha, commit.sha.get(..7).unwrap_or(&commit.sha), theme::mono(11.5), faint);
    if author_w > 0.0 {
        w::avatar(p, pos2(author.left() + 9.0, mid), 8.0, &commit.author, c);
        w::label_in(p, author.with_min_x(author.left() + 23.0), &commit.author, theme::body(12.5), dim);
    }

    let start = row.left() + 4.0 + margin + 6.0;
    let mut x = start;
    let subject_right = author.left() - 10.0;
    let chips = ref_chips(&commit.refs);
    let mut drawn = 0;
    let mut shortened = false;
    for (r, synced) in chips.iter().take(MAX_CHIPS) {
        if x > subject_right - 140.0 {
            break;
        }
        let (icon, fill, on) = match r.kind {
            RefKind::Head => (ic::CHECK, c.primary, c.on_primary),
            RefKind::Local => (ic::BRANCH, c.secondary_container, c.on_secondary_container),
            RefKind::Remote => (ic::CLOUD, c.surface_highest, c.on_surface_variant),
            RefKind::Tag => (ic::TAG, c.tertiary_container, c.on_tertiary_container),
        };
        // A branch in a colored folder wears the folder's color; the remote's name is no folder.
        let path = if r.kind == RefKind::Remote { r.name.split_once('/').map_or("", |(_, rest)| rest) } else { r.name.as_str() };
        let tinted = matches!(r.kind, RefKind::Local | RefKind::Remote).then(|| folder_hue(hues, path)).flatten();
        let (fill, on) = tinted.map_or((fill, on), |hue| (c.tint(hue).1, c.tint(hue).2));
        // The cloud already says "remote": with one remote its name adds nothing.
        let name = match (r.kind == RefKind::Remote && one_remote, r.name.split_once('/')) {
            (true, Some((_, rest))) => rest,
            _ => r.name.as_str(),
        };
        let label = clip_name(name);
        shortened |= label.len() != r.name.len();
        x += w::chip_with(p, pos2(x, mid), icon, &label, if *synced { ic::CLOUD } else { "" }, fill, on) + 5.0;
        drawn += 1;
    }
    if drawn < chips.len() {
        x += w::chip_with(p, pos2(x, mid), "", &format!("+{}", chips.len() - drawn), "", c.surface_highest, c.on_surface_variant) + 5.0;
    }
    let font = if commit.is_head { theme::emph(13.5) } else { theme::body(13.5) };
    w::label_in(p, Rect::from_min_max(pos2(x, row.top()), pos2(subject_right, row.bottom())), &commit.subject, font, fg);

    let over = |from: f32, to: f32| pointer.is_some_and(|at| (from..to).contains(&at.x));
    if over(date.left(), row.right()) {
        return Some(commit.date.clone());
    }
    let hidden = shortened || drawn < chips.len() || chips.len() < commit.refs.len();
    (hidden && over(start, x)).then(|| commit.refs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join("\n"))
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

/// Paints a diff. With `pick` (the picked lines, and whether this is a diff of staged
/// changes) the changes can be staged or unstaged in parts:
/// - a click on a changed line picks it or lets go of it, Shift-click picks up to it,
///   and a drag picks or lets go of every line it passes, as its first line did;
/// - a double click on a changed line, the bar that floats over the diff while lines
///   are picked, or S (U when unstaging) asks for the picked lines;
/// - the button in a hunk's header asks for the whole hunk.
///
/// Returns the lines asked for.
fn paint_diff(ui: &mut Ui, c: &Colors, diff: &Diff, mut pick: Option<(&mut LinePick, bool)>) -> Option<BTreeSet<usize>> {
    const LINE: f32 = 20.0;
    const HUNK: f32 = 34.0;
    /// Room under the last line, so the floating bar never hides it.
    const FOOT: f32 = 72.0;
    let lines = &diff.lines;
    let font = theme::mono(12.5);
    let char_w = w::text_width(ui.painter(), "0", font.clone());
    let digits = lines.iter().rev().find_map(|l| l.new.or(l.old)).unwrap_or(1).to_string().len().max(3) as f32;
    // The column of pick marks, when lines can be picked.
    let marks = if pick.is_some() { 26.0 } else { 0.0 };
    let gutter = marks + (digits * 2.0 + 3.0) * char_w + 16.0;
    let content_w = gutter + (diff.max_chars as f32 + 4.0) * char_w;
    let is_change = |i: usize| matches!(lines[i].kind, LineKind::Added | LineKind::Deleted);
    // Hunk headers are taller than lines, so a row's place depends on the headers above it.
    // ponytail: found anew every frame, keep them with the diff if huge diffs get slow.
    let hunks: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].kind == LineKind::Hunk).collect();
    let top = |i: usize| i as f32 * LINE + hunks.partition_point(|&h| h < i) as f32 * (HUNK - LINE);
    let row_at = |y: f32| {
        let (mut low, mut high) = (0, lines.len().saturating_sub(1));
        while low < high {
            let mid = (low + high) / 2;
            if top(mid + 1) <= y { low = mid + 1 } else { high = mid }
        }
        low
    };
    let (down, pointer, shift) = ui.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos(), i.modifiers.shift));
    let mut wanted = None;
    let body = ui.max_rect();
    ScrollArea::both().id_salt("diff-scroll").auto_shrink(false).show_viewport(ui, |ui, viewport| {
        let origin = ui.cursor().min;
        let width = content_w.max(viewport.width());
        ui.allocate_space(vec2(width, top(lines.len()) + if pick.is_some() { FOOT } else { 0.0 }));
        // What is on screen: headers and buttons stay in it however the diff is scrolled sideways.
        let seen = Rect::from_min_size(origin + viewport.min.to_vec2(), viewport.size());

        if let Some((pick, _)) = &mut pick
            && let Some((from, adding, before)) = pick.drag.clone()
        {
            match pointer.filter(|_| down) {
                None => pick.drag = None,
                Some(at) => {
                    let to = row_at(at.y - origin.y);
                    pick.lines = before;
                    for i in (from.min(to)..=from.max(to)).filter(|&i| is_change(i)) {
                        if adding { pick.lines.insert(i) } else { pick.lines.remove(&i) };
                    }
                    pick.anchor = Some(to);
                    // Past the top or bottom edge the diff scrolls along.
                    let past = (seen.top() - at.y).max(0.0) - (at.y - seen.bottom()).max(0.0);
                    if past != 0.0 {
                        ui.scroll_with_delta(vec2(0.0, past.clamp(-24.0, 24.0)));
                        ui.ctx().request_repaint();
                    }
                }
            }
        }

        let picking = pick.as_ref().is_some_and(|(pick, _)| !pick.lines.is_empty());
        for i in row_at(viewport.min.y)..=row_at(viewport.max.y).min(lines.len().saturating_sub(1)) {
            let line = &lines[i];
            let row = Rect::from_min_size(origin + vec2(0.0, top(i)), vec2(width, if line.kind == LineKind::Hunk { HUNK } else { LINE }));
            let mid = row.center().y;
            if line.kind == LineKind::Hunk {
                let band = Rect::from_min_max(pos2(seen.left() + 4.0, row.top() + 6.0), pos2(seen.right() - 4.0, row.bottom()));
                ui.painter().rect_filled(band, w::radius(shape::SMALL), c.surface_highest);
                // `@@ -1,4 +1,6 @@` is where; what follows it names the function around.
                let (place, around) = line.text.get(2..).and_then(|rest| rest.find("@@")).map_or((line.text.as_str(), ""), |at| line.text.split_at(at + 4));
                let p = ui.painter().with_clip_rect(band.with_max_x(band.right() - if pick.is_some() { 150.0 } else { 0.0 }));
                let place = p.text(pos2(band.left() + 12.0, band.center().y), Align2::LEFT_CENTER, place, theme::mono(11.5), c.outline);
                p.text(pos2(place.right() + 6.0, band.center().y), Align2::LEFT_CENTER, around.trim(), theme::mono(12.0), c.on_surface_variant);
                let Some((_, reverse)) = &pick else { continue };
                let (label, icon, tip) = if *reverse { ("Unstage hunk", ic::REMOVE, "Unstage every change in this hunk") } else { ("Stage hunk", ic::ADD, "Stage every change in this hunk") };
                let pressed = w::hchild(ui, &format!("hunk-{i}"), band.shrink2(vec2(2.0, 0.0)), |ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| w::btn(label).icon(icon).kind(Kind::Text).height(24.0).show(ui).on_hover_text(tip).clicked()).inner
                });
                if pressed {
                    let end = hunks.get(hunks.partition_point(|&h| h <= i)).copied().unwrap_or(lines.len());
                    wanted = Some((i + 1..end).collect());
                }
                continue;
            }
            let (bg, sign, fg) = match line.kind {
                LineKind::Added => (c.diff_added, "+", c.on_surface),
                LineKind::Deleted => (c.diff_deleted, "\u{2212}", c.on_surface),
                LineKind::Note => (Color32::TRANSPARENT, "", c.on_surface_variant),
                _ => (Color32::TRANSPARENT, "", c.on_surface),
            };
            ui.painter().rect_filled(row, 0.0, bg);
            if let Some((pick, _)) = &mut pick {
                // Unchanged lines take part too, so that a drag can begin or end on one.
                let resp = ui.interact(row, ui.scope_id().with(("line", i)), Sense::click_and_drag());
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    pick.drag = Some((i, !pick.lines.contains(&i), pick.lines.clone()));
                }
                if is_change(i) {
                    let hot = resp.hovered() && pick.drag.is_none();
                    if resp.double_clicked() {
                        // The first click of the two may have let go of this line.
                        pick.lines.insert(i);
                        wanted = Some(pick.lines.clone());
                    } else if resp.clicked() {
                        match pick.anchor.filter(|_| shift) {
                            Some(from) => pick.lines.extend((from.min(i)..=from.max(i)).filter(|&j| is_change(j))),
                            None if pick.lines.contains(&i) => drop(pick.lines.remove(&i)),
                            None => drop(pick.lines.insert(i)),
                        }
                        pick.anchor = Some(i);
                    }
                    let p = ui.painter();
                    if hot {
                        p.rect_filled(row, 0.0, c.on_surface.gamma_multiply(0.06));
                    }
                    // The mark: filled when picked, an empty ring where a click would pick.
                    let mark = pos2(row.left() + marks / 2.0 + 1.0, mid);
                    if pick.lines.contains(&i) {
                        p.rect_filled(row.with_max_x(row.left() + 3.0), 0.0, c.primary);
                        p.circle_filled(mark, 7.5, c.primary);
                        w::icon_at(p, mark, ic::CHECK, 12.0, c.on_primary);
                    } else if hot || picking {
                        p.circle_stroke(mark, 6.5, Stroke::new(1.5, if hot { c.on_surface_variant } else { c.outline }));
                    }
                    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
            }
            let p = ui.painter();
            let number = |n: Option<u32>, right: f32| {
                if let Some(n) = n {
                    p.text(pos2(right, mid), Align2::RIGHT_CENTER, n, font.clone(), c.on_surface_variant.gamma_multiply(0.7));
                }
            };
            number(line.old, row.left() + marks + 8.0 + digits * char_w);
            number(line.new, row.left() + marks + 8.0 + (digits * 2.0 + 1.0) * char_w);
            p.text(pos2(row.left() + gutter - 2.0 * char_w, mid), Align2::LEFT_CENTER, sign, font.clone(), fg);
            p.text(pos2(row.left() + gutter, mid), Align2::LEFT_CENTER, &line.text, font.clone(), fg);
        }
    });

    // The picked lines' bar floats over the foot of the diff, and rises into place.
    let Some((pick, reverse)) = pick else { return wanted };
    let count = pick.lines.len();
    let risen = ui.ctx().animate_bool_with_time(ui.scope_id().with("line-bar"), count > 0, 0.16);
    if count == 0 {
        return wanted;
    }
    let (label, icon, key, tip) = if reverse { ("Unstage lines", ic::REMOVE, Key::U, "Unstage the picked lines (U)") } else { ("Stage lines", ic::ADD, Key::S, "Stage the picked lines (S)") };
    if ui.memory(|m| m.focused().is_none()) && ui.input(|i| i.key_pressed(key) && i.modifiers.is_none()) {
        wanted = Some(pick.lines.clone());
    }
    let size = vec2(body.width().min(380.0) - 24.0, 48.0);
    let bar = Rect::from_center_size(pos2(body.center().x, body.bottom() - 16.0 - size.y / 2.0 + (1.0 - risen) * 24.0), size);
    ui.interact(bar, ui.scope_id().with("line-bar"), Sense::click()); // a click on the bar is not one on the line under it
    ui.painter().add(egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(96) }.as_shape(bar, w::radius(24.0)));
    w::card(ui, bar, c.secondary_container, 24.0);
    w::hchild(ui, "line-bar", bar.shrink2(vec2(6.0, 0.0)), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if w::icon_btn(ic::CLOSE, "Let go of the picked lines (Esc)").size(36.0).show(ui).clicked() {
            *pick = LinePick::default();
        }
        let picked = format!("{count} line{} picked", if count == 1 { "" } else { "s" });
        ui.label(egui::RichText::new(picked).font(theme::emph(13.5)).color(c.on_secondary_container));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if w::btn(label).icon(icon).height(36.0).show(ui).on_hover_text(tip).clicked() {
                wanted = Some(pick.lines.clone());
            }
        });
    });
    wanted
}

/// Selection change for a Ctrl or Shift click on `key`. Shift selects the range from the
/// anchor (the last row toggled) through `key`, in display `order`; otherwise the row is
/// toggled and becomes the anchor.
pub(crate) fn apply_selection(marked: &mut BTreeSet<String>, anchor: &mut Option<String>, order: &[String], key: &str, shift: bool) {
    let position = |wanted: &str| order.iter().position(|k| k == wanted);
    if let (true, Some(from), Some(to)) = (shift, anchor.as_deref().and_then(position), position(key)) {
        marked.clear();
        marked.extend(order[from.min(to)..=from.max(to)].iter().cloned());
        return;
    }
    if !marked.remove(key) {
        marked.insert(key.to_string());
    }
    *anchor = Some(key.to_string());
}

/// The delete operation for one marked row.
fn batch_op(group: Group, name: &str, remotes: &[String], force: bool) -> Option<Op> {
    Some(match group {
        Group::Local => Op::DeleteBranch { name: name.into(), force },
        Group::Tag => Op::DeleteTag { name: name.into() },
        Group::Worktree => Op::WorktreeRemove { path: name.into(), force },
        Group::Remote => {
            let remote = remotes.iter().find(|r| name.strip_prefix(r.as_str()).is_some_and(|rest| rest.starts_with('/')))?;
            Op::DeleteRemoteBranch { remote: remote.clone(), name: name[remote.len() + 1..].into() }
        }
    })
}

/// `take` count for the rows of a collapsible section.
fn visible(open: bool) -> usize {
    if open { usize::MAX } else { 0 }
}
