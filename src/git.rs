//! Git backend. Every git invocation in the app goes through [`run`];
//! every mutation is an [`Op`], so the UI and the test suite share one path.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const LOG_FORMAT: &str =
    "--format=%H%x00%P%x00%D%x00%aN%x00%aE%x00%ad%x00%at%x00%s";
const DATE_FORMAT: &str = "--date=format-local:%Y-%m-%d %H:%M";
const DIFF_COMMON: [&str; 4] = ["--no-color", "--no-ext-diff", "--patch", "--unified=4"];

// ---------------------------------------------------------------- process

pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        // core.editor=true: `--continue` style commands accept the prepared message
        // instead of opening an editor the GUI cannot drive.
        .args(["--no-pager", "-c", "core.quotepath=off", "-c", "core.editor=true"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    if cfg!(test) {
        // Hermetic tests: ignore the machine's system/global git config.
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", std::env::temp_dir().join("spoon-no-such-gitconfig"));
    }
    cmd
}

pub fn run<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Output {
    match command(dir).args(args).output() {
        Ok(o) => Output {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => Output {
            ok: false,
            stdout: String::new(),
            stderr: format!("failed to start git in {}: {e}", dir.display()),
        },
    }
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------- models

#[derive(Clone, Debug, PartialEq)]
pub enum RefKind {
    Head,
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ref {
    pub kind: RefKind,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Commit {
    pub sha: String,
    pub parents: Vec<String>,
    pub refs: Vec<Ref>,
    pub author: String,
    pub email: String,
    pub date: String,
    /// Author time in seconds since the Unix epoch, for "3 h ago" labels.
    pub time: i64,
    pub subject: String,
    pub is_head: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Branch {
    /// Short name: `feature/x` for local, `origin/feature/x` for remote.
    pub name: String,
    pub full: String,
    pub remote: Option<String>,
    pub sha: String,
    pub current: bool,
    pub upstream: String,
    pub ahead: u32,
    pub behind: u32,
    pub gone: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tag {
    pub name: String,
    pub sha: String,
    pub annotated: bool,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stash {
    pub sha: String,
    pub name: String,
    pub date: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Worktree {
    pub path: String,
    pub branch: String,
    /// The repository's own work tree; it cannot be removed.
    pub main: bool,
    /// Its folder is gone; `git worktree prune` would drop the entry.
    pub prunable: bool,
}

/// One changed path. `index`/`work` are the porcelain status letters;
/// for commit file lists only `index` is set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    pub path: String,
    pub orig: Option<String>,
    pub index: char,
    pub work: char,
}

impl Change {
    pub fn conflicted(&self) -> bool {
        matches!(
            (self.index, self.work),
            ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
        )
    }
    /// Git's name for the kind of conflict; empty when the path is not in conflict.
    pub fn conflict_reason(&self) -> &'static str {
        match (self.index, self.work) {
            ('D', 'D') => "both deleted",
            ('A', 'U') => "added by us",
            ('U', 'D') => "deleted by them",
            ('U', 'A') => "added by them",
            ('D', 'U') => "deleted by us",
            ('A', 'A') => "both added",
            ('U', 'U') => "both modified",
            _ => "",
        }
    }

    /// Whether resolving with one side means the file goes away, because that side
    /// deleted it or never had it.
    pub fn resolution_deletes(&self, theirs: bool) -> bool {
        match (self.index, self.work) {
            ('D', 'D') => true,
            ('U', 'D') | ('A', 'U') => theirs,
            ('D', 'U') | ('U', 'A') => !theirs,
            _ => false,
        }
    }

    pub fn untracked(&self) -> bool {
        self.index == '?'
    }
    pub fn staged(&self) -> bool {
        !self.conflicted() && !matches!(self.index, ' ' | '?')
    }
    pub fn unstaged(&self) -> bool {
        self.conflicted() || self.work != ' '
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Head {
    Branch(String),
    Detached(String),
}

impl Head {
    pub fn label(&self) -> String {
        match self {
            Head::Branch(b) => b.clone(),
            Head::Detached(sha) => format!("detached @ {sha}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InProgress {
    None,
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl InProgress {
    pub fn label(self) -> &'static str {
        match self {
            InProgress::None => "",
            InProgress::Merge => "Merge",
            InProgress::Rebase => "Rebase",
            InProgress::CherryPick => "Cherry-pick",
            InProgress::Revert => "Revert",
        }
    }
    fn subcommand(self) -> Result<&'static str, String> {
        match self {
            InProgress::None => Err("no operation in progress".into()),
            InProgress::Merge => Ok("merge"),
            InProgress::Rebase => Ok("rebase"),
            InProgress::CherryPick => Ok("cherry-pick"),
            InProgress::Revert => Ok("revert"),
        }
    }
}

// ---------------------------------------------------------------- parsers

pub fn parse_refs(decoration: &str) -> Vec<Ref> {
    let mut refs = Vec::new();
    for part in decoration.split(", ").filter(|p| !p.is_empty()) {
        let r = if let Some(b) = part.strip_prefix("HEAD -> refs/heads/") {
            Ref { kind: RefKind::Head, name: b.into() }
        } else if part == "HEAD" {
            Ref { kind: RefKind::Head, name: "HEAD".into() }
        } else if let Some(t) = part.strip_prefix("tag: refs/tags/") {
            Ref { kind: RefKind::Tag, name: t.into() }
        } else if let Some(b) = part.strip_prefix("refs/heads/") {
            Ref { kind: RefKind::Local, name: b.into() }
        } else if let Some(b) = part.strip_prefix("refs/remotes/") {
            if b.ends_with("/HEAD") {
                continue;
            }
            Ref { kind: RefKind::Remote, name: b.into() }
        } else {
            continue; // refs/stash, refs/notes, ...
        };
        refs.push(r);
    }
    refs
}

pub fn parse_commits(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            if f.len() != 8 {
                return None;
            }
            let refs = parse_refs(f[2]);
            Some(Commit {
                sha: f[0].into(),
                parents: f[1].split_whitespace().map(String::from).collect(),
                is_head: refs.iter().any(|r| r.kind == RefKind::Head),
                refs,
                author: f[3].into(),
                email: f[4].into(),
                date: f[5].into(),
                time: f[6].parse().unwrap_or(0),
                subject: f[7].into(),
            })
        })
        .collect()
}

fn parse_track(track: &str) -> (u32, u32, bool) {
    let num = |key: &str| {
        track
            .split(", ")
            .find_map(|p| p.strip_prefix(key))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };
    (num("ahead "), num("behind "), track == "gone")
}

pub fn parse_branches(out: &str) -> Vec<Branch> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            if f.len() != 5 || f[0].ends_with("/HEAD") {
                return None;
            }
            let (name, remote) = if let Some(n) = f[0].strip_prefix("refs/heads/") {
                (n, None)
            } else {
                let n = f[0].strip_prefix("refs/remotes/")?;
                (n, Some(n.split('/').next()?.to_string()))
            };
            let (ahead, behind, gone) = parse_track(f[4]);
            Some(Branch {
                name: name.into(),
                full: f[0].into(),
                remote,
                sha: f[1].into(),
                current: f[2] == "*",
                upstream: f[3].into(),
                ahead,
                behind,
                gone,
            })
        })
        .collect()
}

pub fn parse_tags(out: &str) -> Vec<Tag> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            if f.len() != 5 {
                return None;
            }
            let annotated = f[1] == "tag";
            Some(Tag {
                name: f[0].into(),
                annotated,
                sha: if f[2].is_empty() { f[3] } else { f[2] }.into(),
                message: if annotated { f[4].into() } else { String::new() },
            })
        })
        .collect()
}

pub fn parse_stashes(out: &str) -> Vec<Stash> {
    out.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            (f.len() == 4).then(|| Stash {
                sha: f[0].into(),
                name: f[1].into(),
                date: f[2].into(),
                message: f[3].into(),
            })
        })
        .collect()
}

pub fn parse_remotes(out: &str) -> Vec<Remote> {
    out.lines()
        .filter_map(|line| {
            let rest = line.strip_suffix(" (fetch)")?;
            let (name, url) = rest.split_once('\t')?;
            Some(Remote { name: name.into(), url: url.into() })
        })
        .collect()
}

/// Parses `git status --porcelain -z`.
pub fn parse_status(out: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut entries = out.split('\0');
    while let Some(e) = entries.next() {
        let bytes = e.as_bytes();
        if bytes.len() < 4 || bytes[2] != b' ' {
            continue;
        }
        let (index, work) = (bytes[0] as char, bytes[1] as char);
        let renamed = matches!(index, 'R' | 'C') || matches!(work, 'R' | 'C');
        changes.push(Change {
            path: e[3..].into(),
            orig: if renamed { entries.next().map(String::from) } else { None },
            index,
            work,
        });
    }
    changes
}

/// Parses `git diff --name-status -z`.
pub fn parse_name_status(out: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut tokens = out.split('\0').filter(|t| !t.is_empty());
    while let Some(status) = tokens.next() {
        let index = status.chars().next().unwrap_or('M');
        let orig = if matches!(index, 'R' | 'C') { tokens.next().map(String::from) } else { None };
        let Some(path) = tokens.next() else { break };
        changes.push(Change { path: path.into(), orig, index, work: ' ' });
    }
    changes
}

pub fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut list: Vec<Worktree> = Vec::new();
    for line in out.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // git lists the main work tree first.
            list.push(Worktree { path: path.into(), branch: "detached".into(), main: list.is_empty(), prunable: false });
        } else if let (Some(b), Some(last)) = (line.strip_prefix("branch refs/heads/"), list.last_mut()) {
            last.branch = b.into();
        } else if let (true, Some(last)) = (line.starts_with("prunable"), list.last_mut()) {
            last.prunable = true;
        }
    }
    list
}

// ---------------------------------------------------------------- queries

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum SearchBy {
    #[default]
    Message,
    Author,
    Path,
    Content,
}

#[derive(Clone, Debug)]
pub struct LogQuery {
    pub limit: usize,
    pub all_branches: bool,
    pub search: Option<(SearchBy, String)>,
}

impl Default for LogQuery {
    fn default() -> Self {
        Self { limit: 5000, all_branches: true, search: None }
    }
}

pub fn log_args(q: &LogQuery) -> Vec<String> {
    let mut a = strings(&["log", "--no-show-signature", "--decorate=full", "--date-order", DATE_FORMAT, LOG_FORMAT]);
    a.push(format!("-n{}", q.limit));
    let mut path = None;
    match &q.search {
        Some((SearchBy::Message, text)) => {
            a.extend(strings(&["-i", "--all-match"]));
            a.extend(text.split_whitespace().map(|w| format!("--grep={w}")));
        }
        Some((SearchBy::Author, text)) => a.extend(["-i".into(), format!("--author={text}")]),
        Some((SearchBy::Content, text)) => a.push(format!("-G{text}")),
        Some((SearchBy::Path, text)) => path = Some(text.clone()),
        None => {}
    }
    if q.all_branches {
        a.extend(strings(&["--branches", "--remotes", "--tags"]));
    }
    a.push("HEAD".into());
    a.push("--".into());
    a.extend(path);
    a
}

pub fn commits(repo: &Path, q: &LogQuery) -> Vec<Commit> {
    parse_commits(&run(repo, &log_args(q)).stdout)
}

pub fn branches(repo: &Path) -> Vec<Branch> {
    let fmt = "--format=%(refname)%00%(objectname)%00%(HEAD)%00%(upstream:short)%00%(upstream:track,nobracket)";
    parse_branches(&run(repo, &["branch", "-l", "--all", fmt]).stdout)
}

pub fn tags(repo: &Path) -> Vec<Tag> {
    let fmt = "--format=%(refname:short)%00%(objecttype)%00%(*objectname)%00%(objectname)%00%(contents:subject)";
    parse_tags(&run(repo, &["tag", "-l", "--sort=-creatordate", fmt]).stdout)
}

pub fn stashes(repo: &Path) -> Vec<Stash> {
    parse_stashes(&run(repo, &["stash", "list", "--format=%H%x00%gd%x00%ci%x00%gs"]).stdout)
}

pub fn remotes(repo: &Path) -> Vec<Remote> {
    parse_remotes(&run(repo, &["remote", "-v"]).stdout)
}

pub fn status(repo: &Path) -> Vec<Change> {
    let args = ["--no-optional-locks", "status", "--porcelain", "-z", "-uall", "--ignore-submodules=dirty"];
    parse_status(&run(repo, &args).stdout)
}

pub fn worktrees(repo: &Path) -> Vec<Worktree> {
    parse_worktrees(&run(repo, &["worktree", "list", "--porcelain"]).stdout)
}

pub fn submodules(repo: &Path) -> Vec<String> {
    run(repo, &["submodule", "status"])
        .stdout
        .lines()
        .filter_map(|l| l.get(1..)?.split_whitespace().nth(1).map(String::from))
        .collect()
}

pub fn head(repo: &Path) -> Head {
    let sym = run(repo, &["symbolic-ref", "--short", "-q", "HEAD"]);
    if sym.ok {
        return Head::Branch(sym.stdout.trim().into());
    }
    Head::Detached(run(repo, &["rev-parse", "--short", "HEAD"]).stdout.trim().into())
}

/// Repository root for any path inside a work tree.
pub fn root(path: &Path) -> Option<PathBuf> {
    let out = run(path, &["rev-parse", "--show-toplevel"]);
    let top = out.stdout.trim();
    (out.ok && !top.is_empty()).then(|| PathBuf::from(top))
}

pub fn in_progress(repo: &Path) -> InProgress {
    let out = run(repo, &["rev-parse", "--absolute-git-dir"]);
    let dir = PathBuf::from(out.stdout.trim());
    let has = |name: &str| dir.join(name).exists();
    if !out.ok {
        InProgress::None
    } else if has("rebase-merge") || has("rebase-apply") {
        InProgress::Rebase
    } else if has("MERGE_HEAD") {
        InProgress::Merge
    } else if has("CHERRY_PICK_HEAD") {
        InProgress::CherryPick
    } else if has("REVERT_HEAD") {
        InProgress::Revert
    } else {
        InProgress::None
    }
}

/// One side of an operation that stopped on conflicts.
#[derive(Clone, Debug, Default)]
pub struct ConflictSide {
    /// Branch name, or a short hash when no branch points at it.
    pub name: String,
    pub tip: Option<Commit>,
    /// Commits that touched the file on this side only, newest first.
    pub commits: Vec<Commit>,
}

/// The two sides in conflict over `path`: (current, incoming).
///
/// - merge, cherry-pick, revert: HEAD against the commit being brought in
/// - rebase: the commit rebased onto against the commit being replayed
/// - no operation (a stash or patch applied with conflicts): HEAD against "Stash or patch"
pub fn conflict_sides(repo: &Path, path: &str) -> (ConflictSide, ConflictSide) {
    let git_dir = PathBuf::from(run(repo, &["rev-parse", "--absolute-git-dir"]).stdout.trim());
    let read = |name: String| std::fs::read_to_string(git_dir.join(name)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let exists = |rev: &str| run(repo, &["rev-parse", "-q", "--verify", rev]).ok;

    // (current revision, incoming revision, name of the branch being replayed)
    let rebase = ["rebase-merge", "rebase-apply"].into_iter().find(|dir| git_dir.join(dir).is_dir());
    let (current, incoming, replayed) = match rebase {
        Some(dir) => {
            let stopped = if exists("REBASE_HEAD") { Some("REBASE_HEAD".to_string()) } else { read(format!("{dir}/stopped-sha")) };
            let branch = read(format!("{dir}/head-name")).map(|name| name.trim_start_matches("refs/heads/").to_string());
            (read(format!("{dir}/onto")).unwrap_or_else(|| "HEAD".into()), stopped, branch)
        }
        None => ("HEAD".to_string(), ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"].into_iter().find(|rev| exists(rev)).map(String::from), None),
    };

    let log = |extra: &[&str]| {
        let mut a = strings(&["log", "--no-show-signature", "--decorate=full", DATE_FORMAT, LOG_FORMAT]);
        a.extend(strings(extra));
        parse_commits(&run(repo, &a).stdout)
    };
    let branch_of = |rev: &str| {
        let refs = run(repo, &["for-each-ref", "--points-at", rev, "--format=%(refname:short)", "refs/heads", "refs/remotes"]);
        match refs.stdout.lines().next() {
            Some(name) => name.to_string(),
            None => run(repo, &["rev-parse", "--short", rev]).stdout.trim().to_string(),
        }
    };
    // Commits on the file that only this side has; without another side, its recent ones.
    let side = |rev: &str, other: Option<&str>, name: String| {
        let range = other.map_or_else(|| rev.to_string(), |other| format!("{other}..{rev}"));
        ConflictSide { name, tip: log(&["-n1", rev, "--"]).into_iter().next(), commits: log(&["-n30", &range, "--", path]) }
    };

    let current_name = if current == "HEAD" { head(repo).label() } else { branch_of(&current) };
    let current_side = side(&current, incoming.as_deref(), current_name);
    let incoming_side = match &incoming {
        Some(rev) => side(rev, Some(&current), replayed.unwrap_or_else(|| branch_of(rev))),
        None => ConflictSide { name: "Stash or patch".into(), ..Default::default() },
    };
    (current_side, incoming_side)
}

pub fn full_message(repo: &Path, sha: &str) -> String {
    run(repo, &["show", "-s", "--no-show-signature", "--format=%B", sha]).stdout.trim_end().into()
}

/// First parent of `commit`, or the empty tree for a root commit.
pub fn base_of(commit: &Commit) -> String {
    commit.parents.first().cloned().unwrap_or_else(|| EMPTY_TREE.into())
}

pub fn changed_files(repo: &Path, base: &str, rev: &str) -> Vec<Change> {
    parse_name_status(&run(repo, &["diff", "--name-status", "-z", "-M", base, rev]).stdout)
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffSpec {
    Unstaged(Change),
    Staged(Change),
    Revs { base: String, rev: String, change: Change },
}

pub fn diff_args(spec: &DiffSpec) -> Vec<String> {
    let mut a = vec!["diff".to_string()];
    let (change, pre): (&Change, Vec<&str>) = match spec {
        DiffSpec::Unstaged(c) if c.untracked() => (c, vec!["--no-index"]),
        DiffSpec::Unstaged(c) => (c, vec![]),
        DiffSpec::Staged(c) => (c, vec!["--cached", "-M"]),
        DiffSpec::Revs { base, rev, change } => (change, vec!["-M", base.as_str(), rev.as_str()]),
    };
    a.extend(DIFF_COMMON.iter().chain(pre.iter()).map(|s| s.to_string()));
    a.push("--".into());
    if matches!(spec, DiffSpec::Unstaged(c) if c.untracked()) {
        a.push("/dev/null".into());
    } else {
        a.extend(change.orig.clone());
    }
    a.push(change.path.clone());
    a
}

/// Shows `spec` in an external diff program. `command` is run by git's shell with the
/// old and the new file added at its end; this returns when the program closes. An
/// untracked file has no old version and cannot be shown.
pub fn difftool(repo: &Path, command: &str, spec: &DiffSpec) -> OpResult {
    let mut args = strings(&["difftool", "--no-prompt"]);
    args.push(format!("--extcmd={command}"));
    // The same revisions and paths as the built-in diff, without its output options.
    args.extend(diff_args(spec).into_iter().skip(1 + DIFF_COMMON.len()));
    let out = run(repo, &args);
    // Git does not pass on the program's exit code, and rightly: many return non-zero
    // for "the files differ". A command the shell could not start shows in its words.
    let missing = ["not found", "No such file", "cannot execute", "Permission denied"].iter().any(|sign| out.stderr.contains(sign));
    OpResult { ok: out.ok && !missing, log: format!("$ git {}\n{}{}", args.join(" "), out.stdout, out.stderr), error: out.stderr }
}

pub fn diff_text(repo: &Path, spec: &DiffSpec) -> String {
    run(repo, &diff_args(spec)).stdout
}

pub fn blame(repo: &Path, rev: &str, path: &str) -> String {
    let mut a = strings(&["blame", "--date=short"]);
    if !rev.is_empty() {
        a.push(rev.into());
    }
    a.extend(["--".into(), path.into()]);
    let out = run(repo, &a);
    if out.ok { out.stdout } else { out.stderr }
}

/// Everything the repository view shows, loaded in parallel.
#[derive(Debug)]
pub struct Snapshot {
    pub head: Head,
    pub state: InProgress,
    pub branches: Vec<Branch>,
    pub tags: Vec<Tag>,
    pub remotes: Vec<Remote>,
    pub stashes: Vec<Stash>,
    pub changes: Vec<Change>,
    pub commits: Vec<Commit>,
    pub submodules: Vec<String>,
    pub worktrees: Vec<Worktree>,
}

pub fn snapshot(repo: &Path, q: &LogQuery) -> Snapshot {
    std::thread::scope(|s| {
        let commits = s.spawn(|| commits(repo, q));
        let branches = s.spawn(|| branches(repo));
        let tags = s.spawn(|| tags(repo));
        let changes = s.spawn(|| status(repo));
        let stashes = s.spawn(|| stashes(repo));
        Snapshot {
            head: head(repo),
            state: in_progress(repo),
            remotes: remotes(repo),
            submodules: submodules(repo),
            worktrees: worktrees(repo),
            commits: h_join(commits),
            branches: h_join(branches),
            tags: h_join(tags),
            changes: h_join(changes),
            stashes: h_join(stashes),
        }
    })
}

fn h_join<T: Default>(h: std::thread::ScopedJoinHandle<'_, T>) -> T {
    h.join().unwrap_or_default()
}

// ---------------------------------------------------------------- operations

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum MergeMode {
    #[default]
    Default,
    NoFf,
    FfOnly,
    Squash,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ResetMode {
    Soft,
    #[default]
    Mixed,
    Hard,
}

/// A repository mutation. `steps` turns it into git argument lists.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Fetch { remote: Option<String>, prune: bool },
    Pull { remote: String, branch: String, rebase: bool, autostash: bool },
    Push { remote: String, local: String, remote_branch: String, force: bool, set_upstream: bool, tags: bool },
    PushTag { remote: String, tag: String, delete: bool },
    Checkout { target: String },
    CheckoutRemote { remote_branch: String, local: String },
    CreateBranch { name: String, base: String, checkout: bool },
    DeleteBranch { name: String, force: bool },
    DeleteRemoteBranch { remote: String, name: String },
    RenameBranch { old: String, new: String },
    Merge { source: String, mode: MergeMode },
    Rebase { onto: String, autostash: bool },
    CherryPick { sha: String, mainline: bool },
    Revert { sha: String, mainline: bool },
    Reset { target: String, mode: ResetMode },
    CreateTag { name: String, target: String, message: String },
    DeleteTag { name: String },
    StashPush { message: String, include_untracked: bool },
    StashApply { name: String, pop: bool },
    StashDrop { name: String },
    Stage { paths: Vec<String> },
    StageAll,
    Unstage { paths: Vec<String> },
    UnstageAll,
    Discard { changes: Vec<Change> },
    Commit { message: String, amend: bool, signoff: bool },
    AddRemote { name: String, url: String },
    RemoveRemote { name: String },
    Continue(InProgress),
    Abort(InProgress),
    Skip,
    /// Settles a conflict with one side's version, whatever kind of conflict it is.
    Resolve { change: Change, theirs: bool },
    SubmoduleUpdate,
    /// Checks out `target` in a new folder; with `new_branch`, on a new branch started there.
    WorktreeAdd { path: String, target: String, new_branch: Option<String> },
    WorktreeRemove { path: String, force: bool },
    WorktreePrune,
}

/// Rejects values git would read as an option or that cannot be a name.
fn arg(value: &str, what: &str) -> Result<String, String> {
    let v = value.trim();
    if v.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if v.starts_with('-') {
        return Err(format!("{what} must not start with '-'"));
    }
    if v.chars().any(char::is_control) {
        return Err(format!("{what} contains control characters"));
    }
    Ok(v.to_string())
}

/// Splits a path list so no command line exceeds the OS limit (32k chars on Windows).
fn chunked(prefix: &[&str], paths: &[String]) -> Vec<Vec<String>> {
    let mut steps = Vec::new();
    let mut current = strings(prefix);
    let mut len = 0;
    for p in paths {
        if len + p.len() > 8000 && current.len() > prefix.len() {
            steps.push(std::mem::replace(&mut current, strings(prefix)));
            len = 0;
        }
        len += p.len() + 1;
        current.push(p.clone());
    }
    if current.len() > prefix.len() {
        steps.push(current);
    }
    steps
}

fn flagged(base: &[&str], flags: &[(bool, &str)], tail: &[String]) -> Vec<String> {
    let mut a = strings(base);
    a.extend(flags.iter().filter(|(on, _)| *on).map(|(_, f)| f.to_string()));
    a.extend_from_slice(tail);
    a
}

impl Op {
    pub fn title(&self) -> &'static str {
        match self {
            Op::Fetch { .. } => "Fetch",
            Op::Pull { .. } => "Pull",
            Op::Push { .. } => "Push",
            Op::PushTag { delete: false, .. } => "Push tag",
            Op::PushTag { delete: true, .. } => "Delete remote tag",
            Op::Checkout { .. } | Op::CheckoutRemote { .. } => "Checkout",
            Op::CreateBranch { .. } => "Create branch",
            Op::DeleteBranch { .. } | Op::DeleteRemoteBranch { .. } => "Delete branch",
            Op::RenameBranch { .. } => "Rename branch",
            Op::Merge { .. } => "Merge",
            Op::Rebase { .. } => "Rebase",
            Op::CherryPick { .. } => "Cherry-pick",
            Op::Revert { .. } => "Revert",
            Op::Reset { .. } => "Reset",
            Op::CreateTag { .. } => "Create tag",
            Op::DeleteTag { .. } => "Delete tag",
            Op::StashPush { .. } => "Stash",
            Op::StashApply { pop: false, .. } => "Apply stash",
            Op::StashApply { pop: true, .. } => "Pop stash",
            Op::StashDrop { .. } => "Drop stash",
            Op::Stage { .. } | Op::StageAll => "Stage",
            Op::Unstage { .. } | Op::UnstageAll => "Unstage",
            Op::Discard { .. } => "Discard",
            Op::Commit { .. } => "Commit",
            Op::AddRemote { .. } => "Add remote",
            Op::RemoveRemote { .. } => "Remove remote",
            Op::Continue(_) => "Continue",
            Op::Abort(_) => "Abort",
            Op::Skip => "Skip",
            Op::Resolve { .. } => "Resolve conflict",
            Op::SubmoduleUpdate => "Update submodules",
            Op::WorktreeAdd { .. } => "Add worktree",
            Op::WorktreeRemove { .. } => "Remove worktree",
            Op::WorktreePrune => "Prune worktrees",
        }
    }

    pub fn steps(&self) -> Result<Vec<Vec<String>>, String> {
        let one = |v: Vec<String>| Ok(vec![v]);
        match self {
            Op::Fetch { remote, prune } => {
                let target = match remote {
                    Some(r) => arg(r, "remote")?,
                    None => "--all".into(),
                };
                one(flagged(&["fetch", "--progress"], &[(*prune, "--prune")], &[target]))
            }
            Op::Pull { remote, branch, rebase, autostash } => {
                let flags = [(*rebase, "--rebase"), (!*rebase, "--no-rebase"), (*autostash, "--autostash")];
                one(flagged(&["pull", "--progress"], &flags, &[arg(remote, "remote")?, arg(branch, "branch")?]))
            }
            Op::Push { remote, local, remote_branch, force, set_upstream, tags } => {
                let spec = format!("refs/heads/{}:refs/heads/{}", arg(local, "local branch")?, arg(remote_branch, "remote branch")?);
                let flags = [(*force, "--force-with-lease"), (*set_upstream, "-u"), (*tags, "--tags")];
                one(flagged(&["push", "--progress"], &flags, &[arg(remote, "remote")?, spec]))
            }
            Op::PushTag { remote, tag, delete } => {
                let spec = format!("refs/tags/{}", arg(tag, "tag")?);
                one(flagged(&["push"], &[(*delete, "--delete")], &[arg(remote, "remote")?, spec]))
            }
            Op::Checkout { target } => one(vec!["checkout".into(), arg(target, "target")?]),
            Op::CheckoutRemote { remote_branch, local } => one(vec![
                "checkout".into(),
                "-b".into(),
                arg(local, "branch name")?,
                "--track".into(),
                arg(remote_branch, "remote branch")?,
            ]),
            Op::CreateBranch { name, base, checkout } => {
                let (name, base) = (arg(name, "branch name")?, arg(base, "base")?);
                one(if *checkout {
                    vec!["checkout".into(), "-b".into(), name, base]
                } else {
                    vec!["branch".into(), name, base]
                })
            }
            Op::DeleteBranch { name, force } => {
                let flag = if *force { "-D" } else { "-d" };
                one(vec!["branch".into(), flag.into(), arg(name, "branch")?])
            }
            Op::DeleteRemoteBranch { remote, name } => one(vec![
                "push".into(),
                arg(remote, "remote")?,
                "--delete".into(),
                format!("refs/heads/{}", arg(name, "branch")?),
            ]),
            Op::RenameBranch { old, new } => {
                one(vec!["branch".into(), "-m".into(), arg(old, "branch")?, arg(new, "new name")?])
            }
            Op::Merge { source, mode } => {
                let mode = match mode {
                    MergeMode::Default => "--ff",
                    MergeMode::NoFf => "--no-ff",
                    MergeMode::FfOnly => "--ff-only",
                    MergeMode::Squash => "--squash",
                };
                one(vec!["merge".into(), "--no-edit".into(), mode.into(), arg(source, "source")?])
            }
            Op::Rebase { onto, autostash } => {
                one(flagged(&["rebase"], &[(*autostash, "--autostash")], &[arg(onto, "target")?]))
            }
            Op::CherryPick { sha, mainline } => {
                one(flagged(&["cherry-pick"], &[(*mainline, "-m1")], &[arg(sha, "commit")?]))
            }
            Op::Revert { sha, mainline } => {
                one(flagged(&["revert", "--no-edit"], &[(*mainline, "-m1")], &[arg(sha, "commit")?]))
            }
            Op::Reset { target, mode } => {
                let mode = match mode {
                    ResetMode::Soft => "--soft",
                    ResetMode::Mixed => "--mixed",
                    ResetMode::Hard => "--hard",
                };
                one(vec!["reset".into(), mode.into(), arg(target, "target")?])
            }
            Op::CreateTag { name, target, message } => {
                let (name, target) = (arg(name, "tag name")?, arg(target, "target")?);
                one(if message.trim().is_empty() {
                    vec!["tag".into(), name, target]
                } else {
                    vec!["tag".into(), "-a".into(), "-m".into(), message.trim().into(), name, target]
                })
            }
            Op::DeleteTag { name } => one(vec!["tag".into(), "-d".into(), arg(name, "tag")?]),
            Op::StashPush { message, include_untracked } => {
                let mut a = flagged(&["stash", "push"], &[(*include_untracked, "--include-untracked")], &[]);
                if !message.trim().is_empty() {
                    a.extend(["-m".into(), message.trim().into()]);
                }
                one(a)
            }
            Op::StashApply { name, pop } => {
                let verb = if *pop { "pop" } else { "apply" };
                one(vec!["stash".into(), verb.into(), arg(name, "stash")?])
            }
            Op::StashDrop { name } => one(vec!["stash".into(), "drop".into(), arg(name, "stash")?]),
            Op::Stage { paths } => Ok(chunked(&["add", "-A", "--"], paths)),
            Op::StageAll => one(strings(&["add", "-A"])),
            Op::Unstage { paths } => Ok(chunked(&["reset", "-q", "--"], paths)),
            Op::UnstageAll => one(strings(&["reset", "-q"])),
            Op::Discard { changes } => {
                let paths = |untracked: bool| -> Vec<String> {
                    changes.iter().filter(|c| c.untracked() == untracked).map(|c| c.path.clone()).collect()
                };
                let mut steps = chunked(&["restore", "--"], &paths(false));
                steps.extend(chunked(&["clean", "-f", "-d", "--"], &paths(true)));
                Ok(steps)
            }
            Op::Commit { message, amend, signoff } => {
                let message = message.trim();
                let mut a = flagged(&["commit"], &[(*amend, "--amend"), (*signoff, "--signoff")], &[]);
                match (message.is_empty(), amend) {
                    (true, false) => return Err("commit message is empty".into()),
                    (true, true) => a.push("--no-edit".into()),
                    (false, _) => a.extend(["-m".into(), message.into()]),
                }
                one(a)
            }
            Op::AddRemote { name, url } => {
                one(vec!["remote".into(), "add".into(), arg(name, "remote name")?, arg(url, "url")?])
            }
            Op::RemoveRemote { name } => one(vec!["remote".into(), "remove".into(), arg(name, "remote")?]),
            Op::Continue(state) => one(strings(&[state.subcommand()?, "--continue"])),
            Op::Abort(state) => one(strings(&[state.subcommand()?, "--abort"])),
            Op::Skip => one(strings(&["rebase", "--skip"])),
            Op::Resolve { change, theirs } => {
                let path = &change.path;
                if change.resolution_deletes(*theirs) {
                    // That side has no such file: taking it removes the file.
                    return one(vec!["rm".into(), "-q".into(), "--".into(), path.clone()]);
                }
                let side = if *theirs { "--theirs" } else { "--ours" };
                Ok(vec![
                    vec!["checkout".into(), side.into(), "--".into(), path.clone()],
                    vec!["add".into(), "--".into(), path.clone()],
                ])
            }
            Op::SubmoduleUpdate => one(strings(&["submodule", "update", "--init", "--recursive"])),
            Op::WorktreeAdd { path, target, new_branch } => {
                let branch = new_branch.as_ref().map(|name| arg(name, "branch name")).transpose()?;
                let (target, path) = (arg(target, "branch")?, arg(path, "folder")?);
                // git creates the new branch before it looks at the folder, and leaves the
                // branch behind when the folder is unusable. So check the folder first.
                let folder = Path::new(&path);
                if !folder.is_absolute() {
                    return Err("worktree folder must be an absolute path".into());
                }
                if folder.is_file() || folder.read_dir().is_ok_and(|mut entries| entries.next().is_some()) {
                    return Err(format!("{path} already exists and is not empty"));
                }
                let mut a = strings(&["worktree", "add"]);
                if let Some(name) = branch {
                    a.extend(["-b".into(), name]);
                }
                a.extend([path, target]);
                one(a)
            }
            Op::WorktreeRemove { path, force } => {
                one(flagged(&["worktree", "remove"], &[(*force, "--force")], &[arg(path, "worktree")?]))
            }
            Op::WorktreePrune => one(strings(&["worktree", "prune"])),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpResult {
    pub ok: bool,
    /// Transcript: `$ git ...` followed by the command output, per step.
    pub log: String,
    pub error: String,
}

fn run_steps(dir: &Path, steps: &[Vec<String>]) -> OpResult {
    let mut result = OpResult { ok: true, ..Default::default() };
    for step in steps {
        let out = run(dir, step);
        result.log += &format!("$ git {}\n{}{}\n", step.join(" "), out.stdout, out.stderr);
        if !out.ok {
            result.ok = false;
            let err = if out.stderr.trim().is_empty() { &out.stdout } else { &out.stderr };
            result.error = err.lines().filter(|l| !l.starts_with("hint:")).collect::<Vec<_>>().join("\n");
            break;
        }
    }
    result
}

pub fn run_op(repo: &Path, op: &Op) -> OpResult {
    match op.steps() {
        Ok(steps) => run_steps(repo, &steps),
        Err(error) => OpResult { ok: false, log: format!("{}: {error}\n", op.title()), error },
    }
}

/// Runs `ops` in order. Normally the first failure stops the run (the later steps
/// depend on it). With `keep_going` the operations are independent, as in a batch
/// delete: all of them run and every failure is reported.
pub fn run_ops(repo: &Path, ops: &[Op], keep_going: bool) -> OpResult {
    let mut total = OpResult { ok: true, ..Default::default() };
    let mut errors = Vec::new();
    for op in ops {
        let result = run_op(repo, op);
        total.log += &result.log;
        if !result.ok {
            errors.push(result.error.trim().to_string());
            if !keep_going {
                break;
            }
        }
    }
    total.ok = errors.is_empty();
    total.error = if keep_going && !errors.is_empty() {
        format!("{} of {} failed:\n{}", errors.len(), ops.len(), errors.join("\n"))
    } else {
        errors.join("\n")
    };
    total
}

pub fn init(path: &Path) -> OpResult {
    if let Err(e) = std::fs::create_dir_all(path) {
        let error = format!("cannot create {}: {e}", path.display());
        return OpResult { ok: false, log: error.clone(), error };
    }
    run_steps(path, &[strings(&["init", "-q"])])
}

pub fn clone(url: &str, parent: &Path, name: &str) -> OpResult {
    let args = || -> Result<Vec<String>, String> {
        let mut a = strings(&["clone", "--progress", "--recurse-submodules", "--"]);
        a.push(arg(url, "url")?);
        if !name.trim().is_empty() {
            a.push(arg(name, "folder name")?);
        }
        Ok(a)
    };
    match args() {
        Ok(a) => run_steps(parent, &[a]),
        Err(error) => OpResult { ok: false, log: format!("Clone: {error}\n"), error },
    }
}

pub fn short(sha: &str) -> &str {
    sha.get(..10).unwrap_or(sha)
}
