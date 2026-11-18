//! Workflow tests: drive real git in throwaway repositories through the same
//! `Op` / query API the UI uses, then assert on the resulting repository state.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::git::{self, Change, DiffSpec, Head, InProgress, LogQuery, MergeMode, Op, RefKind, ResetMode, SearchBy};
use crate::conflict::{self, Document, FileState, Segment, Side, Take};
use crate::{diff, graph};

// ---------------------------------------------------------------- harness

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("spoon-test-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    /// Empty repository on an unborn `main`.
    fn repo(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        assert!(git::init(&path).ok);
        g(&path, &["checkout", "-q", "-b", "main"]);
        configure(&path);
        path
    }

    fn bare(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(&path).unwrap();
        g(&path, &["init", "-q", "--bare"]);
        g(&path, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        path
    }

    fn clone(&self, origin: &Path, name: &str) -> PathBuf {
        let result = git::clone(origin.to_str().unwrap(), &self.root, name);
        assert!(result.ok, "clone failed: {}", result.error);
        let path = self.root.join(name);
        configure(&path);
        path
    }

    /// Bare `origin` plus clone `a`, with one pushed commit on `main`.
    fn with_origin(&self) -> (PathBuf, PathBuf) {
        let origin = self.bare("origin.git");
        let a = self.clone(&origin, "a");
        g(&a, &["checkout", "-q", "-B", "main"]);
        commit(&a, "f.txt", "one\n", "first");
        op(&a, push("main", true));
        (origin, a)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn configure(repo: &Path) {
    let config = [
        ("user.name", "Spoon Test"),
        ("user.email", "test@spoon.invalid"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ];
    for (key, value) in config {
        g(repo, &["config", key, value]);
    }
}

/// Raw git for arranging and inspecting; must succeed.
fn g(repo: &Path, args: &[&str]) -> String {
    let out = git::run(repo, args);
    assert!(out.ok, "git {args:?} failed: {}", out.stderr);
    out.stdout.trim().to_string()
}

fn op(repo: &Path, op: Op) {
    let result = git::run_op(repo, &op);
    assert!(result.ok, "{op:?} failed: {}\n{}", result.error, result.log);
}

fn fails(repo: &Path, op: Op) -> String {
    let result = git::run_op(repo, &op);
    assert!(!result.ok, "{op:?} unexpectedly succeeded");
    assert!(!result.error.is_empty(), "{op:?} failed without an error message");
    result.error
}

fn write(repo: &Path, file: &str, content: &str) {
    let path = repo.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn read(repo: &Path, file: &str) -> String {
    fs::read_to_string(repo.join(file)).unwrap()
}

fn stage(paths: &[&str]) -> Op {
    Op::Stage { paths: paths.iter().map(|p| p.to_string()).collect() }
}

fn commit_op(message: &str) -> Op {
    Op::Commit { message: message.into(), amend: false, signoff: false }
}

fn commit(repo: &Path, file: &str, content: &str, message: &str) -> String {
    write(repo, file, content);
    op(repo, stage(&[file]));
    op(repo, commit_op(message));
    sha(repo, "HEAD")
}

fn sha(repo: &Path, rev: &str) -> String {
    g(repo, &["rev-parse", rev])
}

fn checkout(target: &str) -> Op {
    Op::Checkout { target: target.into() }
}

fn branch(name: &str, checkout: bool) -> Op {
    Op::CreateBranch { name: name.into(), base: "HEAD".into(), checkout }
}

fn merge(source: &str, mode: MergeMode) -> Op {
    Op::Merge { source: source.into(), mode }
}

fn push(branch: &str, set_upstream: bool) -> Op {
    Op::Push {
        remote: "origin".into(),
        local: branch.into(),
        remote_branch: branch.into(),
        force: false,
        set_upstream,
        tags: false,
    }
}

fn pull(rebase: bool) -> Op {
    Op::Pull { remote: "origin".into(), branch: "main".into(), rebase, autostash: false }
}

fn fetch() -> Op {
    Op::Fetch { remote: None, prune: true }
}

fn log(repo: &Path) -> Vec<git::Commit> {
    git::commits(repo, &LogQuery::default())
}

fn subjects(repo: &Path) -> Vec<String> {
    log(repo).into_iter().map(|c| c.subject).collect()
}

/// Subject of HEAD. Unlike `subjects(..)[0]` this does not depend on how git orders
/// unrelated branch tips committed within the same second.
fn head_subject(repo: &Path) -> String {
    g(repo, &["log", "-1", "--format=%s"])
}

fn change<'a>(changes: &'a [Change], path: &str) -> &'a Change {
    changes.iter().find(|c| c.path == path).unwrap_or_else(|| panic!("no change for {path} in {changes:?}"))
}

/// The conflicted change for `path`, as the UI would hand it to `Op::Resolve`.
fn conflict_of(repo: &Path, path: &str) -> Change {
    let found = change(&git::status(repo), path).clone();
    assert!(found.conflicted(), "{path} is not in conflict: {found:?}");
    found
}

fn current(repo: &Path) -> String {
    match git::head(repo) {
        Head::Branch(name) => name,
        Head::Detached(sha) => panic!("detached at {sha}"),
    }
}

fn local<'a>(branches: &'a [git::Branch], name: &str) -> &'a git::Branch {
    branches.iter().find(|b| b.remote.is_none() && b.name == name).unwrap_or_else(|| panic!("no branch {name}"))
}

fn branch_names(repo: &Path) -> Vec<String> {
    git::branches(repo).into_iter().map(|b| b.name).collect()
}

/// `main` and `other` both rewrite f.txt. Leaves the repo on `main`.
fn diverge(repo: &Path) {
    commit(repo, "f.txt", "base\n", "base");
    op(repo, branch("other", true));
    commit(repo, "f.txt", "theirs\n", "theirs change");
    op(repo, checkout("main"));
    commit(repo, "f.txt", "ours\n", "ours change");
}

// ---------------------------------------------------------------- working copy

#[test]
fn status_reports_every_kind_of_change() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    for file in ["mod.txt", "del.txt", "old.txt", "sp ace.txt", "ünï/ço.txt"] {
        write(&repo, file, "v1\n");
    }
    op(&repo, Op::StageAll);
    op(&repo, commit_op("init"));

    write(&repo, "mod.txt", "v2\n");
    fs::remove_file(repo.join("del.txt")).unwrap();
    g(&repo, &["mv", "old.txt", "renamed.txt"]);
    write(&repo, "sp ace.txt", "v2\n");
    op(&repo, stage(&["sp ace.txt"]));
    write(&repo, "sp ace.txt", "v3\n");
    write(&repo, "ünï/ço.txt", "v2\n");
    write(&repo, "new dir/untracked.txt", "x\n");

    let changes = git::status(&repo);
    assert_eq!(changes.len(), 6, "{changes:?}");
    let m = change(&changes, "mod.txt");
    assert!((m.index, m.work) == (' ', 'M') && m.unstaged() && !m.staged());
    let d = change(&changes, "del.txt");
    assert!((d.index, d.work) == (' ', 'D') && d.unstaged());
    let r = change(&changes, "renamed.txt");
    assert!(r.index == 'R' && r.orig.as_deref() == Some("old.txt") && r.staged() && !r.unstaged());
    let both = change(&changes, "sp ace.txt");
    assert!((both.index, both.work) == ('M', 'M') && both.staged() && both.unstaged());
    assert!(change(&changes, "ünï/ço.txt").unstaged());
    let u = change(&changes, "new dir/untracked.txt");
    assert!(u.untracked() && u.unstaged() && !u.staged());
}

#[test]
fn stage_and_unstage_before_first_commit() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    write(&repo, "a.txt", "a\n");
    write(&repo, "b.txt", "b\n");

    op(&repo, stage(&["a.txt"]));
    let changes = git::status(&repo);
    assert!(change(&changes, "a.txt").staged());
    assert!(change(&changes, "b.txt").untracked());

    op(&repo, Op::Unstage { paths: vec!["a.txt".into()] });
    assert!(git::status(&repo).iter().all(Change::untracked));

    op(&repo, Op::StageAll);
    assert!(git::status(&repo).iter().all(Change::staged));
    op(&repo, Op::UnstageAll);
    assert!(git::status(&repo).iter().all(Change::untracked));
}

#[test]
fn stage_handles_deletions_renames_and_dash_names() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    write(&repo, "gone.txt", "x\n");
    write(&repo, "old.txt", "content that survives a rename\n");
    op(&repo, Op::StageAll);
    op(&repo, commit_op("init"));

    fs::remove_file(repo.join("gone.txt")).unwrap();
    op(&repo, stage(&["gone.txt"]));
    assert_eq!(change(&git::status(&repo), "gone.txt").index, 'D');

    // A file whose name looks like an option must be treated as a path.
    write(&repo, "-rf.txt", "not a rename of gone.txt\n");
    op(&repo, stage(&["-rf.txt"]));
    assert_eq!(change(&git::status(&repo), "-rf.txt").index, 'A');
    op(&repo, Op::Unstage { paths: vec!["-rf.txt".into()] });
    let dash = change(&git::status(&repo), "-rf.txt").clone();
    assert!(dash.untracked());
    op(&repo, Op::Discard { changes: vec![dash] });
    assert!(!repo.join("-rf.txt").exists());

    // Unstaging a staged rename needs both sides.
    g(&repo, &["mv", "old.txt", "new.txt"]);
    op(&repo, Op::Unstage { paths: vec!["new.txt".into(), "old.txt".into()] });
    let changes = git::status(&repo);
    assert_eq!(change(&changes, "old.txt").work, 'D');
    assert!(change(&changes, "new.txt").untracked());
}

#[test]
fn stage_survives_huge_path_lists() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let long = "d".repeat(80);
    let paths: Vec<String> = (0..600).map(|i| format!("{long}/file-{i:04}.txt")).collect();
    for p in &paths {
        write(&repo, p, "x\n");
    }
    let steps = Op::Stage { paths: paths.clone() }.steps().unwrap();
    assert!(steps.len() > 1, "expected chunking");
    assert!(steps.iter().all(|s| s.join(" ").len() < 12_000));
    assert_eq!(steps.iter().map(|s| s.len() - 3).sum::<usize>(), paths.len());

    op(&repo, Op::Stage { paths: paths.clone() });
    let changes = git::status(&repo);
    assert_eq!(changes.len(), 600);
    assert!(changes.iter().all(Change::staged));
    op(&repo, Op::Unstage { paths });
    assert!(git::status(&repo).iter().all(Change::untracked));
}

#[test]
fn commit_amend_and_message_rules() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    write(&repo, "a.txt", "a\n");
    op(&repo, Op::StageAll);

    assert!(fails(&repo, commit_op("   ")).contains("empty"));
    assert!(log(&repo).is_empty());

    op(&repo, commit_op("-starts with dash\n\nbody line"));
    assert_eq!(subjects(&repo), ["-starts with dash"]);
    assert_eq!(git::full_message(&repo, "HEAD"), "-starts with dash\n\nbody line");

    // Amend with a new message replaces the commit.
    let before = sha(&repo, "HEAD");
    write(&repo, "b.txt", "b\n");
    op(&repo, Op::StageAll);
    op(&repo, Op::Commit { message: "reworded".into(), amend: true, signoff: false });
    assert_eq!(subjects(&repo), ["reworded"]);
    assert_ne!(sha(&repo, "HEAD"), before);
    assert_eq!(git::changed_files(&repo, git::EMPTY_TREE, "HEAD").len(), 2);

    // Amend without a message keeps it.
    write(&repo, "c.txt", "c\n");
    op(&repo, Op::StageAll);
    op(&repo, Op::Commit { message: String::new(), amend: true, signoff: false });
    assert_eq!(subjects(&repo), ["reworded"]);

    op(&repo, Op::Commit { message: "signed".into(), amend: true, signoff: true });
    assert!(git::full_message(&repo, "HEAD").contains("Signed-off-by: Spoon Test <test@spoon.invalid>"));

    // Nothing staged: git refuses, and the error reaches the caller.
    fails(&repo, commit_op("nothing to commit"));
}

#[test]
fn discard_restores_exactly_the_selected_changes() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    for file in ["keep.txt", "mod.txt", "del.txt", "half.txt"] {
        write(&repo, file, "v1\n");
    }
    op(&repo, Op::StageAll);
    op(&repo, commit_op("init"));

    write(&repo, "keep.txt", "edited but not discarded\n");
    write(&repo, "mod.txt", "v2\n");
    fs::remove_file(repo.join("del.txt")).unwrap();
    write(&repo, "half.txt", "staged\n");
    op(&repo, stage(&["half.txt"]));
    write(&repo, "half.txt", "staged + worktree\n");
    write(&repo, "junk/deep/a.txt", "x\n");
    write(&repo, "junk.txt", "x\n");

    let changes = git::status(&repo);
    let pick = |p: &str| change(&changes, p).clone();
    op(&repo, Op::Discard { changes: vec![pick("mod.txt"), pick("del.txt"), pick("half.txt"), pick("junk/deep/a.txt"), pick("junk.txt")] });

    assert_eq!(read(&repo, "mod.txt"), "v1\n");
    assert_eq!(read(&repo, "del.txt"), "v1\n");
    assert_eq!(read(&repo, "half.txt"), "staged\n", "discard must keep the staged version");
    // git clean removes the files; emptied directories are invisible to git and may stay.
    assert!(!repo.join("junk/deep/a.txt").exists() && !repo.join("junk.txt").exists());
    assert_eq!(read(&repo, "keep.txt"), "edited but not discarded\n");

    let after = git::status(&repo);
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(change(&after, "half.txt").staged() && !change(&after, "half.txt").unstaged());
    assert!(change(&after, "keep.txt").unstaged());
}

// ---------------------------------------------------------------- branches

#[test]
fn branch_lifecycle() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let first = commit(&repo, "a.txt", "a\n", "first");

    op(&repo, branch("feature/login", false));
    assert_eq!(current(&repo), "main");
    assert!(branch_names(&repo).contains(&"feature/login".to_string()));

    op(&repo, checkout("feature/login"));
    assert_eq!(current(&repo), "feature/login");
    let branches = git::branches(&repo);
    assert!(local(&branches, "feature/login").current && !local(&branches, "main").current);
    assert_eq!(local(&branches, "main").sha, first);

    op(&repo, Op::RenameBranch { old: "feature/login".into(), new: "feature/auth".into() });
    assert_eq!(current(&repo), "feature/auth");

    commit(&repo, "b.txt", "b\n", "unmerged work");
    op(&repo, checkout("main"));
    fails(&repo, Op::DeleteBranch { name: "feature/auth".into(), force: false });
    assert!(branch_names(&repo).contains(&"feature/auth".to_string()), "unmerged branch must survive a safe delete");
    op(&repo, Op::DeleteBranch { name: "feature/auth".into(), force: true });
    assert_eq!(branch_names(&repo), ["main"]);

    fails(&repo, Op::DeleteBranch { name: "main".into(), force: true }); // checked out
    fails(&repo, branch("main", false)); // already exists
    fails(&repo, checkout("does-not-exist"));

    op(&repo, Op::CreateBranch { name: "from-sha".into(), base: first.clone(), checkout: true });
    assert_eq!((current(&repo), sha(&repo, "HEAD")), ("from-sha".to_string(), first));
}

#[test]
fn checkout_refuses_to_clobber_local_changes() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "main\n", "first");
    op(&repo, branch("other", true));
    commit(&repo, "a.txt", "other\n", "other");
    op(&repo, checkout("main"));

    write(&repo, "a.txt", "precious local edit\n");
    fails(&repo, checkout("other"));
    assert_eq!(current(&repo), "main");
    assert_eq!(read(&repo, "a.txt"), "precious local edit\n");
}

#[test]
fn detached_head_is_reported() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let first = commit(&repo, "a.txt", "1\n", "first");
    commit(&repo, "a.txt", "2\n", "second");

    op(&repo, checkout(&first));
    assert_eq!(git::head(&repo), Head::Detached(g(&repo, &["rev-parse", "--short", "HEAD"])));
    let commits = log(&repo);
    let head = commits.iter().find(|c| c.is_head).unwrap();
    assert_eq!(head.sha, first);
    assert!(git::branches(&repo).iter().all(|b| !b.current), "no branch is current while detached");

    op(&repo, checkout("main"));
    assert_eq!(current(&repo), "main");
}

// ---------------------------------------------------------------- merge

#[test]
fn merge_modes() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "base");
    op(&repo, branch("feat", true));
    let tip = commit(&repo, "b.txt", "b\n", "feature work");
    op(&repo, checkout("main"));

    // Fast-forward: main simply moves.
    op(&repo, merge("feat", MergeMode::Default));
    assert_eq!(sha(&repo, "HEAD"), tip);

    // --no-ff: a real merge commit with two parents.
    op(&repo, Op::Reset { target: "HEAD~1".into(), mode: ResetMode::Hard });
    op(&repo, merge("feat", MergeMode::NoFf));
    let top = &log(&repo)[0];
    assert_eq!(top.parents.len(), 2);
    assert_eq!(top.parents[1], tip);
    assert_eq!(top.subject, "Merge branch 'feat'");

    // --squash: changes staged, no commit, no merge state.
    op(&repo, Op::Reset { target: "HEAD~1".into(), mode: ResetMode::Hard });
    let before = sha(&repo, "HEAD");
    op(&repo, merge("feat", MergeMode::Squash));
    assert_eq!(sha(&repo, "HEAD"), before);
    assert!(change(&git::status(&repo), "b.txt").staged());
    assert_eq!(git::in_progress(&repo), InProgress::None);
    op(&repo, Op::Reset { target: "HEAD".into(), mode: ResetMode::Hard });

    // --ff-only refuses when histories diverged.
    commit(&repo, "c.txt", "c\n", "main moves on");
    let before = sha(&repo, "HEAD");
    fails(&repo, merge("feat", MergeMode::FfOnly));
    assert_eq!(sha(&repo, "HEAD"), before);
    assert_eq!(git::in_progress(&repo), InProgress::None);
}

#[test]
fn merge_conflict_abort_restores_everything() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    diverge(&repo);
    let before = sha(&repo, "HEAD");

    let error = fails(&repo, merge("other", MergeMode::Default));
    assert!(error.contains("CONFLICT") || error.contains("conflict"), "{error}");
    assert_eq!(git::in_progress(&repo), InProgress::Merge);
    let changes = git::status(&repo);
    let conflict = change(&changes, "f.txt");
    assert!(conflict.conflicted() && conflict.unstaged() && !conflict.staged());
    assert!(read(&repo, "f.txt").contains("<<<<<<<"));

    op(&repo, Op::Abort(InProgress::Merge));
    assert_eq!(git::in_progress(&repo), InProgress::None);
    assert_eq!(sha(&repo, "HEAD"), before);
    assert_eq!(read(&repo, "f.txt"), "ours\n");
    assert!(git::status(&repo).is_empty());
}

#[test]
fn merge_conflict_resolve_ours_or_theirs_then_continue() {
    for (theirs, expected) in [(false, "ours\n"), (true, "theirs\n")] {
        let sb = Sandbox::new();
        let repo = sb.repo("r");
        diverge(&repo);
        let (ours_tip, theirs_tip) = (sha(&repo, "main"), sha(&repo, "other"));

        fails(&repo, merge("other", MergeMode::Default));
        // Continuing with unresolved conflicts must fail and keep the merge alive.
        fails(&repo, Op::Continue(InProgress::Merge));
        assert_eq!(git::in_progress(&repo), InProgress::Merge);

        op(&repo, Op::Resolve { change: conflict_of(&repo, "f.txt"), theirs });
        assert!(!git::status(&repo).iter().any(Change::conflicted));
        op(&repo, Op::Continue(InProgress::Merge));

        assert_eq!(git::in_progress(&repo), InProgress::None);
        assert_eq!(read(&repo, "f.txt"), expected);
        assert_eq!(log(&repo)[0].parents, [ours_tip, theirs_tip]);
        assert!(git::status(&repo).is_empty());
    }
}

// ---------------------------------------------------------------- rebase

#[test]
fn rebase_replays_commits_linearly() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "base");
    op(&repo, branch("feat", true));
    commit(&repo, "b.txt", "b\n", "feat 1");
    commit(&repo, "c.txt", "c\n", "feat 2");
    op(&repo, checkout("main"));
    let main_tip = commit(&repo, "d.txt", "d\n", "main work");
    op(&repo, checkout("feat"));

    op(&repo, Op::Rebase { onto: "main".into(), autostash: false });
    assert_eq!(subjects(&repo), ["feat 2", "feat 1", "main work", "base"]);
    assert!(log(&repo).iter().all(|c| c.parents.len() <= 1));
    assert_eq!(sha(&repo, "HEAD~2"), main_tip);
    assert_eq!(current(&repo), "feat");
}

#[test]
fn rebase_autostash_carries_local_edits() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "base");
    op(&repo, branch("feat", true));
    commit(&repo, "b.txt", "b\n", "feat");
    op(&repo, checkout("main"));
    commit(&repo, "c.txt", "c\n", "main work");
    op(&repo, checkout("feat"));
    write(&repo, "b.txt", "uncommitted\n");

    fails(&repo, Op::Rebase { onto: "main".into(), autostash: false });
    assert_eq!(read(&repo, "b.txt"), "uncommitted\n");

    op(&repo, Op::Rebase { onto: "main".into(), autostash: true });
    assert_eq!(read(&repo, "b.txt"), "uncommitted\n");
    assert_eq!(subjects(&repo), ["feat", "main work", "base"]);
    assert!(git::stashes(&repo).is_empty());
}

#[test]
fn rebase_conflict_abort_continue_and_skip() {
    // Abort: branch returns to where it was.
    let sb = Sandbox::new();
    let repo = sb.repo("abort");
    diverge(&repo);
    let before = sha(&repo, "HEAD");
    fails(&repo, Op::Rebase { onto: "other".into(), autostash: false });
    assert_eq!(git::in_progress(&repo), InProgress::Rebase);
    assert!(change(&git::status(&repo), "f.txt").conflicted());
    op(&repo, Op::Abort(InProgress::Rebase));
    assert_eq!((git::in_progress(&repo), sha(&repo, "HEAD"), current(&repo)), (InProgress::None, before, "main".into()));
    assert_eq!(read(&repo, "f.txt"), "ours\n");

    // Continue: during a rebase "theirs" is the commit being replayed.
    let repo = sb.repo("continue");
    diverge(&repo);
    fails(&repo, Op::Rebase { onto: "other".into(), autostash: false });
    fails(&repo, Op::Continue(InProgress::Rebase));
    op(&repo, Op::Resolve { change: conflict_of(&repo, "f.txt"), theirs: true });
    op(&repo, Op::Continue(InProgress::Rebase));
    assert_eq!(git::in_progress(&repo), InProgress::None);
    assert_eq!(subjects(&repo), ["ours change", "theirs change", "base"]);
    assert_eq!(read(&repo, "f.txt"), "ours\n");
    assert_eq!(current(&repo), "main");

    // Skip: the conflicting commit is dropped.
    let repo = sb.repo("skip");
    diverge(&repo);
    fails(&repo, Op::Rebase { onto: "other".into(), autostash: false });
    op(&repo, Op::Skip);
    assert_eq!(git::in_progress(&repo), InProgress::None);
    assert_eq!(sha(&repo, "HEAD"), sha(&repo, "other"));
    assert_eq!(read(&repo, "f.txt"), "theirs\n");
}

// ---------------------------------------------------------------- cherry-pick / revert / reset

#[test]
fn cherry_pick_plain_conflicting_and_merge_commits() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "base");
    op(&repo, branch("feat", true));
    let pick = commit(&repo, "b.txt", "b\n", "pick me");
    let clash = commit(&repo, "a.txt", "feat a\n", "clash");
    op(&repo, checkout("main"));
    commit(&repo, "a.txt", "main a\n", "main a");

    op(&repo, Op::CherryPick { sha: pick.clone(), mainline: false });
    assert_eq!(head_subject(&repo), "pick me");
    assert_ne!(sha(&repo, "HEAD"), pick, "cherry-pick creates a new commit");
    assert_eq!(read(&repo, "b.txt"), "b\n");

    let before = sha(&repo, "HEAD");
    fails(&repo, Op::CherryPick { sha: clash, mainline: false });
    assert_eq!(git::in_progress(&repo), InProgress::CherryPick);
    op(&repo, Op::Abort(InProgress::CherryPick));
    assert_eq!((git::in_progress(&repo), sha(&repo, "HEAD")), (InProgress::None, before));
    assert_eq!(read(&repo, "a.txt"), "main a\n");

    // Merge commits need a mainline.
    op(&repo, branch("side", true));
    commit(&repo, "s.txt", "s\n", "side");
    op(&repo, checkout("main"));
    op(&repo, merge("side", MergeMode::NoFf));
    let merge_commit = sha(&repo, "HEAD");
    op(&repo, Op::Reset { target: "HEAD~1".into(), mode: ResetMode::Hard });
    fails(&repo, Op::CherryPick { sha: merge_commit.clone(), mainline: false });
    assert_eq!(git::in_progress(&repo), InProgress::None);
    op(&repo, Op::CherryPick { sha: merge_commit, mainline: true });
    assert_eq!(read(&repo, "s.txt"), "s\n");
}

#[test]
fn revert_plain_and_conflicting() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "1\n", "one");
    let two = commit(&repo, "a.txt", "2\n", "two");
    commit(&repo, "b.txt", "b\n", "unrelated");

    op(&repo, Op::Revert { sha: two.clone(), mainline: false });
    assert_eq!(subjects(&repo)[0], "Revert \"two\"");
    assert_eq!(read(&repo, "a.txt"), "1\n");
    assert_eq!(read(&repo, "b.txt"), "b\n");

    // Reverting "one" now conflicts: the line changed again since.
    commit(&repo, "a.txt", "3\n", "three");
    let before = sha(&repo, "HEAD");
    fails(&repo, Op::Revert { sha: two, mainline: false });
    assert_eq!(git::in_progress(&repo), InProgress::Revert);
    op(&repo, Op::Abort(InProgress::Revert));
    assert_eq!((git::in_progress(&repo), sha(&repo, "HEAD")), (InProgress::None, before));
    assert_eq!(read(&repo, "a.txt"), "3\n");
}

#[test]
fn reset_modes() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let first = commit(&repo, "a.txt", "1\n", "one");
    let reset = |mode| Op::Reset { target: first.clone(), mode };
    let redo = |repo: &Path| {
        g(repo, &["reset", "-q", "--hard", &first]);
        commit(repo, "a.txt", "2\n", "two");
    };

    redo(&repo);
    op(&repo, reset(ResetMode::Soft));
    assert_eq!(sha(&repo, "HEAD"), first);
    assert_eq!(change(&git::status(&repo), "a.txt").index, 'M', "soft keeps changes staged");

    redo(&repo);
    op(&repo, reset(ResetMode::Mixed));
    let changes = git::status(&repo);
    assert_eq!((change(&changes, "a.txt").index, change(&changes, "a.txt").work), (' ', 'M'), "mixed keeps changes unstaged");
    assert_eq!(read(&repo, "a.txt"), "2\n");

    redo(&repo);
    write(&repo, "a.txt", "dirty\n");
    op(&repo, reset(ResetMode::Hard));
    assert_eq!(sha(&repo, "HEAD"), first);
    assert_eq!(read(&repo, "a.txt"), "1\n");
    assert!(git::status(&repo).is_empty());
}

// ---------------------------------------------------------------- tags / stash

#[test]
fn tags_lightweight_and_annotated() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let first = commit(&repo, "a.txt", "1\n", "one");
    let second = commit(&repo, "a.txt", "2\n", "two");

    op(&repo, Op::CreateTag { name: "v1".into(), target: first.clone(), message: String::new() });
    op(&repo, Op::CreateTag { name: "v2".into(), target: "HEAD".into(), message: "release two".into() });
    fails(&repo, Op::CreateTag { name: "v1".into(), target: "HEAD".into(), message: String::new() });

    let tags = git::tags(&repo);
    let tag = |n: &str| tags.iter().find(|t| t.name == n).unwrap();
    assert_eq!((tag("v1").sha.as_str(), tag("v1").annotated), (first.as_str(), false));
    assert_eq!((tag("v2").sha.as_str(), tag("v2").annotated), (second.as_str(), true), "annotated tag resolves to its commit");
    assert_eq!(tag("v2").message, "release two");

    let commits = log(&repo);
    assert!(commits[0].refs.contains(&git::Ref { kind: RefKind::Tag, name: "v2".into() }));
    assert!(commits[1].refs.contains(&git::Ref { kind: RefKind::Tag, name: "v1".into() }));

    op(&repo, Op::DeleteTag { name: "v1".into() });
    assert_eq!(git::tags(&repo).len(), 1);
    fails(&repo, Op::DeleteTag { name: "v1".into() });
}

#[test]
fn stash_push_apply_pop_drop() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "1\n", "one");

    write(&repo, "a.txt", "wip\n");
    write(&repo, "new.txt", "untracked\n");
    op(&repo, Op::StashPush { message: "my wip".into(), include_untracked: false });
    assert_eq!(read(&repo, "a.txt"), "1\n");
    assert!(repo.join("new.txt").exists(), "untracked file stays without include_untracked");

    op(&repo, Op::StashPush { message: String::new(), include_untracked: true });
    assert!(!repo.join("new.txt").exists());
    assert!(git::status(&repo).is_empty());

    let stashes = git::stashes(&repo);
    assert_eq!(stashes.len(), 2);
    assert_eq!(stashes[0].name, "stash@{0}");
    assert_eq!(stashes[1].name, "stash@{1}");
    assert!(stashes[1].message.ends_with("my wip"), "{:?}", stashes[1]);
    let files = git::changed_files(&repo, &format!("{}^", stashes[1].sha), &stashes[1].sha);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "a.txt");

    // Apply keeps the stash, pop removes it.
    op(&repo, Op::StashApply { name: "stash@{1}".into(), pop: false });
    assert_eq!(read(&repo, "a.txt"), "wip\n");
    assert_eq!(git::stashes(&repo).len(), 2);
    g(&repo, &["checkout", "--", "a.txt"]);

    op(&repo, Op::StashApply { name: "stash@{0}".into(), pop: true });
    assert_eq!(read(&repo, "new.txt"), "untracked\n");
    assert_eq!(git::stashes(&repo).len(), 1);

    op(&repo, Op::StashDrop { name: "stash@{0}".into() });
    assert!(git::stashes(&repo).is_empty());
    fails(&repo, Op::StashDrop { name: "stash@{0}".into() });

    // Nothing to stash is not silently reported as a new stash.
    g(&repo, &["clean", "-fdq"]);
    let _ = git::run_op(&repo, &Op::StashPush { message: String::new(), include_untracked: true });
    assert!(git::stashes(&repo).is_empty());
}

#[test]
fn stash_pop_conflict_keeps_the_stash() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "1\n", "one");
    write(&repo, "a.txt", "stashed\n");
    op(&repo, Op::StashPush { message: String::new(), include_untracked: false });
    commit(&repo, "a.txt", "2\n", "two");

    fails(&repo, Op::StashApply { name: "stash@{0}".into(), pop: true });
    assert_eq!(git::stashes(&repo).len(), 1, "a failed pop must not lose the stash");
}

// ---------------------------------------------------------------- worktrees

#[test]
fn worktrees_add_remove_and_prune() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "first");
    op(&repo, branch("feat", false));
    let folder = |name: &str| sb.root.join(name).to_str().unwrap().replace('\\', "/");
    let tree_of = |branch: &str| git::worktrees(&repo).into_iter().find(|t| t.branch == branch).unwrap_or_else(|| panic!("no worktree for {branch}"));

    // An existing branch, and a new branch started at HEAD.
    op(&repo, Op::WorktreeAdd { path: folder("wt-feat"), target: "feat".into(), new_branch: None });
    op(&repo, Op::WorktreeAdd { path: folder("wt-topic"), target: "HEAD".into(), new_branch: Some("topic".into()) });
    let trees = git::worktrees(&repo);
    assert_eq!(trees.len(), 3);
    assert!(trees[0].main && trees[0].branch == "main");
    let feat = tree_of("feat");
    assert!(!feat.main && !feat.prunable);
    let feat_dir = PathBuf::from(&feat.path);
    assert_eq!(read(&feat_dir, "a.txt"), "a\n");
    assert!(branch_names(&repo).contains(&"topic".to_string()));
    assert_eq!(current(&repo), "main", "adding a worktree leaves the main checkout alone");

    // A branch can be checked out only once, and the folder must be free.
    fails(&repo, Op::WorktreeAdd { path: folder("wt-dup"), target: "feat".into(), new_branch: None });
    // git itself would create the branch `other` and then fail on the folder; Spoon must not.
    let taken = fails(&repo, Op::WorktreeAdd { path: folder("wt-feat"), target: "HEAD".into(), new_branch: Some("other".into()) });
    assert!(taken.contains("already exists"), "{taken}");
    assert!(fails(&repo, Op::WorktreeAdd { path: "relative/dir".into(), target: "feat".into(), new_branch: None }).contains("absolute"));
    assert_eq!(git::worktrees(&repo).len(), 3);
    assert!(!branch_names(&repo).contains(&"other".to_string()), "a rejected add must not leave a branch behind");

    // Work committed in a worktree lands on its branch in the shared repository.
    commit(&feat_dir, "f.txt", "f\n", "work in worktree");
    assert_eq!(g(&repo, &["log", "-1", "--format=%s", "feat"]), "work in worktree");
    assert_eq!(git::root(&feat_dir).map(|p| p.join("f.txt").exists()), Some(true));

    // Removing refuses to throw away uncommitted work unless forced.
    write(&feat_dir, "unsaved.txt", "precious\n");
    fails(&repo, Op::WorktreeRemove { path: feat.path.clone(), force: false });
    assert_eq!(read(&feat_dir, "unsaved.txt"), "precious\n");
    assert_eq!(git::worktrees(&repo).len(), 3);
    op(&repo, Op::WorktreeRemove { path: feat.path.clone(), force: true });
    assert!(!feat_dir.exists());
    assert_eq!(g(&repo, &["log", "-1", "--format=%s", "feat"]), "work in worktree", "removing a worktree keeps its branch and commits");

    // The main work tree is never removable.
    fails(&repo, Op::WorktreeRemove { path: trees[0].path.clone(), force: true });
    assert_eq!(read(&repo, "a.txt"), "a\n");

    // A clean worktree goes without force.
    op(&repo, Op::WorktreeAdd { path: folder("wt-clean"), target: "feat".into(), new_branch: None });
    op(&repo, Op::WorktreeRemove { path: tree_of("feat").path, force: false });

    // A folder deleted by hand leaves a stale entry until pruned.
    fs::remove_dir_all(tree_of("topic").path).unwrap();
    assert!(tree_of("topic").prunable);
    op(&repo, Op::WorktreePrune);
    let left = git::worktrees(&repo);
    assert_eq!(left.len(), 1);
    assert!(left[0].main && !left[0].prunable);
    assert!(branch_names(&repo).contains(&"topic".to_string()), "pruning keeps the branch");
}

// ---------------------------------------------------------------- conflict editor

const TWO_CONFLICTS: &str = "intro\n<<<<<<< HEAD\nours 1\nours 2\n=======\ntheirs 1\n>>>>>>> feature\nmiddle\n<<<<<<< HEAD\nA\n||||||| base\nO\n=======\nB\nC\n>>>>>>> feature\noutro\n";

fn nth_conflict(doc: &mut Document, n: usize) -> &mut conflict::Conflict {
    doc.conflicts_mut().nth(n).unwrap()
}

#[test]
fn conflict_document_parses_and_round_trips() {
    let doc = Document::parse(TWO_CONFLICTS);
    assert_eq!((doc.total(), doc.resolved(), doc.segments.len()), (2, 0, 5));
    assert!(matches!(&doc.segments[0], Segment::Plain(lines) if lines == &["intro"]));
    let first = doc.conflicts().next().unwrap();
    assert_eq!(first.current, ["ours 1", "ours 2"]);
    assert_eq!(first.incoming, ["theirs 1"]);
    assert_eq!((first.current_label.as_str(), first.incoming_label.as_str()), ("HEAD", "feature"));
    let second = doc.conflicts().nth(1).unwrap();
    assert_eq!((second.current.len(), second.incoming.len()), (1, 2), "the diff3 base section is not a side");

    // An untouched document renders byte for byte, whatever its line endings.
    let samples = [
        TWO_CONFLICTS.to_string(),
        TWO_CONFLICTS.replace('\n', "\r\n"),
        TWO_CONFLICTS.trim_end().to_string(),
        String::new(),
        "no conflicts\nhere\n".into(),
        "<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b".into(),
        "\n\n<<<<<<< HEAD\n=======\n>>>>>>> b\n\n".into(),
    ];
    for text in samples {
        assert_eq!(Document::parse(&text).render(), text, "{text:?}");
    }

    // Marker look-alikes and cut-off blocks stay plain text and survive unchanged.
    let not_conflicts = ["=======\ntitle\n=======\n", "<<<<<<< HEAD\nx\n=======\ny\n", "<<<<<<<HEAD\nx\n=======\ny\n>>>>>>>b\n", ">>>>>>> stray\n", "<<<<<<< HEAD\nx\n>>>>>>> b\n"];
    for text in not_conflicts {
        let doc = Document::parse(text);
        assert_eq!((doc.total(), doc.render().as_str()), (0, text));
    }
    let nested = Document::parse("<<<<<<< a\n<<<<<<< b\nx\n=======\ny\n>>>>>>> c\n");
    assert_eq!(nested.total(), 1, "the inner, complete block is the conflict");
    assert_eq!(nested.conflicts().next().unwrap().current_label, "b");
}

#[test]
fn conflict_take_resolves_whole_blocks() {
    let mut doc = Document::parse(TWO_CONFLICTS);
    nth_conflict(&mut doc, 0).take(Take::Incoming);
    assert_eq!((doc.total(), doc.resolved()), (2, 1));
    assert_eq!(doc.render(), "intro\ntheirs 1\nmiddle\n<<<<<<< HEAD\nA\n||||||| base\nO\n=======\nB\nC\n>>>>>>> feature\noutro\n", "unresolved blocks keep their markers");

    nth_conflict(&mut doc, 1).take(Take::Both);
    assert_eq!(doc.render(), "intro\ntheirs 1\nmiddle\nA\nB\nC\noutro\n");
    nth_conflict(&mut doc, 0).take(Take::Current);
    nth_conflict(&mut doc, 1).take(Take::Neither);
    assert_eq!(doc.render(), "intro\nours 1\nours 2\nmiddle\noutro\n");
    assert_eq!(doc.resolved(), 2);
    nth_conflict(&mut doc, 1).reset();
    assert_eq!(doc.resolved(), 1);
    assert!(doc.render().contains("||||||| base"));

    let mut crlf = Document::parse(&TWO_CONFLICTS.replace('\n', "\r\n"));
    crlf.conflicts_mut().for_each(|c| c.take(Take::Current));
    assert_eq!(crlf.render(), "intro\r\nours 1\r\nours 2\r\nmiddle\r\nA\r\noutro\r\n");

    let mut everything = Document::parse("<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> b\n");
    nth_conflict(&mut everything, 0).take(Take::Neither);
    assert_eq!(everything.render(), "", "a file resolved to nothing is empty, not a lone newline");
}

#[test]
fn conflict_lines_are_picked_individually() {
    let mut doc = Document::parse("<<<<<<< HEAD\na1\na2\na3\n=======\nb1\nb2\n>>>>>>> x\n");
    let c = nth_conflict(&mut doc, 0);

    // The side picked first leads; each side keeps its own order whatever the click order.
    c.toggle(Side::Incoming, 1);
    assert!(c.resolved && c.is_picked(Side::Incoming, 1) && !c.is_picked(Side::Incoming, 0));
    c.toggle(Side::Current, 2);
    c.toggle(Side::Current, 0);
    c.toggle(Side::Incoming, 0);
    assert_eq!(c.result, "b1\nb2\na1\na3\n");
    c.toggle(Side::Current, 0);
    assert_eq!(c.result, "b1\nb2\na3\n");

    // Unpicking everything makes the conflict unresolved again.
    for (side, index) in [(Side::Incoming, 0), (Side::Incoming, 1), (Side::Current, 2)] {
        c.toggle(side, index);
    }
    assert!(!c.resolved && c.result.is_empty());

    // Starting from the other side flips the order.
    c.toggle(Side::Current, 1);
    c.toggle(Side::Incoming, 0);
    assert_eq!(c.result, "a2\nb1\n");

    // A range, given in either direction, picks every line once.
    c.reset();
    c.toggle(Side::Current, 2);
    c.pick_range(Side::Current, 2, 0);
    assert_eq!(c.result, "a1\na2\na3\n");
    c.toggle(Side::Current, 99);
    assert_eq!(c.result, "a1\na2\na3\n", "an index past the end is ignored");

    // The chunk checkbox: a whole side on or off, leaving the other side alone.
    c.reset();
    c.toggle(Side::Incoming, 1);
    c.set_side(Side::Current, true);
    assert_eq!((c.picked_count(Side::Current), c.picked_count(Side::Incoming)), (3, 1));
    assert_eq!(c.result, "b2\na1\na2\na3\n");
    c.set_side(Side::Current, true);
    assert_eq!(c.result, "b2\na1\na2\na3\n", "already picked lines are not toggled off");
    c.set_side(Side::Current, false);
    assert_eq!((c.result.as_str(), c.resolved, c.dropped()), ("b2\n", true, false));
    c.set_side(Side::Incoming, false);
    assert!(!c.resolved && !c.dropped());
    c.take(Take::Neither);
    assert!(c.resolved && c.dropped());

    // A blank line is a real line.
    let mut blank = Document::parse("<<<<<<< HEAD\n\nx\n=======\ny\n>>>>>>> b\nend\n");
    nth_conflict(&mut blank, 0).toggle(Side::Current, 0);
    assert_eq!(blank.render(), "\nend\n");
}

#[test]
fn conflict_result_can_be_edited() {
    let mut doc = Document::parse("top\n<<<<<<< HEAD\na1\na2\n=======\nb1\n>>>>>>> x\n");
    let c = nth_conflict(&mut doc, 0);
    c.toggle(Side::Current, 0);
    c.edit("a1\n"); // identical text is not an edit
    c.toggle(Side::Incoming, 0);
    assert_eq!(c.result, "a1\nb1\n", "still generated from the picks");

    // After typing, picks add a line at the end and unpicks remove only that line.
    c.edit("a1 and b1 merged by hand");
    assert!(c.resolved);
    c.toggle(Side::Current, 1);
    assert_eq!(c.result, "a1 and b1 merged by hand\na2\n");
    c.toggle(Side::Current, 1);
    assert_eq!(c.result, "a1 and b1 merged by hand\n");
    c.toggle(Side::Current, 0);
    assert_eq!(c.result, "a1 and b1 merged by hand\n", "a line edited away has nothing to remove");
    assert_eq!(doc.render(), "top\na1 and b1 merged by hand\n");

    // Clearing the text is a valid resolution; reset returns to the markers.
    nth_conflict(&mut doc, 0).edit("");
    assert_eq!((doc.resolved(), doc.render().as_str()), (1, "top\n"));
    nth_conflict(&mut doc, 0).reset();
    assert!(doc.render().contains("<<<<<<< HEAD"));

    // A whole-block choice replaces hand edits.
    let c = nth_conflict(&mut doc, 0);
    c.edit("junk");
    c.take(Take::Both);
    assert_eq!(c.result, "a1\na2\nb1\n");
}

#[test]
fn conflict_editor_resolves_a_real_merge() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let file = |two: &str, six: &str| format!("one\n{two}\nc1\nc2\nc3\nc4\nc5\n{six}\n");
    commit(&repo, "f.txt", &file("two", "six"), "base");
    commit(&repo, "gone.txt", "x\n", "another file");
    op(&repo, branch("other", true));
    commit(&repo, "f.txt", &file("TWO theirs", "SIX theirs"), "theirs");
    g(&repo, &["rm", "-q", "gone.txt"]);
    op(&repo, commit_op("theirs deletes"));
    op(&repo, checkout("main"));
    commit(&repo, "f.txt", &file("TWO ours", "SIX ours"), "ours");
    commit(&repo, "gone.txt", "changed\n", "ours edits");
    fails(&repo, merge("other", MergeMode::Default));

    // Both sides are described for the conflict panel: branch, tip, commits on the file.
    let (current, incoming) = git::conflict_sides(&repo, "f.txt");
    assert_eq!((current.name.as_str(), incoming.name.as_str()), ("main", "other"));
    let subjects = |side: &git::ConflictSide| side.commits.iter().map(|c| c.subject.clone()).collect::<Vec<_>>();
    assert_eq!((subjects(&current), subjects(&incoming)), (vec!["ours".to_string()], vec!["theirs".to_string()]));
    assert_eq!(current.tip.unwrap().subject, "ours edits");
    assert_eq!(incoming.tip.unwrap().subject, "theirs deletes");

    // A file one side deleted has no markers: the editor declines and says what to do.
    assert!(conflict::load(&repo, "gone.txt").unwrap_err().contains("no conflict markers"));
    assert!(conflict::load(&repo, "missing.txt").unwrap_err().contains("not in the work tree"));

    let (original, mut doc) = conflict::load(&repo, "f.txt").unwrap();
    assert_eq!(doc.total(), 2);
    assert_eq!(doc.conflicts().next().unwrap().incoming_label, "other");
    nth_conflict(&mut doc, 0).toggle(Side::Incoming, 0);
    let second = nth_conflict(&mut doc, 1);
    second.take(Take::Current);
    second.edit("SIX merged by hand\n");
    let resolved = file("TWO theirs", "SIX merged by hand");
    assert_eq!(doc.render(), resolved);

    // Never overwrite a file that changed on disk since it was opened.
    write(&repo, "f.txt", "edited elsewhere\n");
    assert!(conflict::save(&repo, "f.txt", &original, &doc.render()).unwrap_err().contains("changed on disk"));
    assert_eq!(read(&repo, "f.txt"), "edited elsewhere\n");
    write(&repo, "f.txt", &original);

    conflict::save(&repo, "f.txt", &original, &doc.render()).unwrap();
    assert_eq!(read(&repo, "f.txt"), resolved);
    op(&repo, stage(&["f.txt"]));
    op(&repo, Op::Resolve { change: conflict_of(&repo, "gone.txt"), theirs: true });
    assert!(!git::status(&repo).iter().any(Change::conflicted));
    op(&repo, Op::Continue(InProgress::Merge));

    assert_eq!(git::in_progress(&repo), InProgress::None);
    assert_eq!(git::conflict_sides(&repo, "f.txt").1.name, "Stash or patch", "no incoming commit once the merge is done");
    assert_eq!(log(&repo)[0].parents.len(), 2);
    assert_eq!(read(&repo, "f.txt"), resolved);
    assert_eq!(g(&repo, &["show", "HEAD:f.txt"]) + "\n", resolved, "the merge commit holds the resolved text");
    assert!(!repo.join("gone.txt").exists(), "taking the deleting side removes the file");
    assert!(git::status(&repo).is_empty());
}

/// A merge that stops with every kind of conflict git reports, plus a binary one.
/// Leaves the repo on `main`, mid-merge.
fn every_conflict_kind(repo: &Path) {
    let renamed = "a file that both sides rename\nkeeps enough lines\nto be detected\nas the same file\n";
    for (file, text) in [("uu.txt", "base\n"), ("ud.txt", "base\n"), ("du.txt", "base\n"), ("bin.dat", "\0base"), ("ren.txt", renamed)] {
        write(repo, file, text);
    }
    op(repo, Op::StageAll);
    op(repo, commit_op("base"));
    op(repo, branch("other", true));
    for (file, text) in [("uu.txt", "theirs\n"), ("aa.txt", "theirs added\n"), ("du.txt", "theirs changed\n"), ("bin.dat", "\0theirs")] {
        write(repo, file, text);
    }
    g(repo, &["rm", "-q", "ud.txt"]);
    g(repo, &["mv", "ren.txt", "theirs-name.txt"]);
    op(repo, Op::StageAll);
    op(repo, commit_op("theirs"));
    op(repo, checkout("main"));
    for (file, text) in [("uu.txt", "ours\n"), ("aa.txt", "ours added\n"), ("ud.txt", "ours changed\n"), ("bin.dat", "\0ours")] {
        write(repo, file, text);
    }
    g(repo, &["rm", "-q", "du.txt"]);
    g(repo, &["mv", "ren.txt", "ours-name.txt"]);
    op(repo, Op::StageAll);
    op(repo, commit_op("ours"));
    fails(repo, merge("other", MergeMode::Default));
}

#[test]
fn every_conflict_kind_is_named_and_classified() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    every_conflict_kind(&repo);
    let changes = git::status(&repo);
    let kind = |path: &str| {
        let found = change(&changes, path);
        (format!("{}{}", found.index, found.work), found.conflict_reason(), conflict::file_state(&repo, found))
    };
    assert_eq!(kind("uu.txt"), ("UU".into(), "both modified", FileState::Text));
    assert_eq!(kind("aa.txt"), ("AA".into(), "both added", FileState::Text));
    assert_eq!(kind("bin.dat"), ("UU".into(), "both modified", FileState::Binary));
    assert_eq!(kind("ud.txt"), ("UD".into(), "deleted by them", FileState::OneSided));
    assert_eq!(kind("du.txt"), ("DU".into(), "deleted by us", FileState::OneSided));
    assert_eq!(kind("ren.txt"), ("DD".into(), "both deleted", FileState::OneSided));
    assert_eq!(kind("ours-name.txt"), ("AU".into(), "added by us", FileState::OneSided));
    assert_eq!(kind("theirs-name.txt"), ("UA".into(), "added by them", FileState::OneSided));
    assert_eq!(changes.iter().filter(|c| c.conflicted()).count(), 8);
    assert_eq!(Change::default().conflict_reason(), "");

    // The editor takes text conflicts only, and says why otherwise.
    assert!(conflict::load(&repo, "uu.txt").is_ok());
    assert!(conflict::load(&repo, "bin.dat").unwrap_err().contains("binary file"));
    assert!(conflict::load(&repo, "ud.txt").unwrap_err().contains("no conflict markers"));
    assert!(conflict::load(&repo, "ren.txt").unwrap_err().contains("not in the work tree"));

    // Markers removed in another editor: nothing left to merge, only to stage.
    write(&repo, "uu.txt", "fixed by hand\n");
    assert_eq!(conflict::file_state(&repo, change(&changes, "uu.txt")), FileState::Resolved);
}

#[test]
fn every_conflict_kind_resolves_to_either_side() {
    for theirs in [false, true] {
        let sb = Sandbox::new();
        let repo = sb.repo("r");
        every_conflict_kind(&repo);
        for conflicted in git::status(&repo).into_iter().filter(|c| c.conflicted()) {
            let deletes = conflicted.resolution_deletes(theirs);
            op(&repo, Op::Resolve { change: conflicted.clone(), theirs });
            assert_eq!(repo.join(&conflicted.path).exists(), !deletes, "{} with theirs={theirs}", conflicted.path);
        }
        assert!(!git::status(&repo).iter().any(Change::conflicted));
        op(&repo, Op::Continue(InProgress::Merge));
        assert_eq!((git::in_progress(&repo), log(&repo)[0].parents.len()), (InProgress::None, 2));
        assert!(git::status(&repo).is_empty());

        let exists = |file: &str| repo.join(file).exists();
        let (kept, dropped) = if theirs { ("theirs-name.txt", "ours-name.txt") } else { ("ours-name.txt", "theirs-name.txt") };
        assert!(exists(kept) && !exists(dropped) && !exists("ren.txt"), "rename conflict keeps one name (theirs={theirs})");
        if theirs {
            assert_eq!((read(&repo, "uu.txt"), read(&repo, "aa.txt"), read(&repo, "du.txt")), ("theirs\n".into(), "theirs added\n".into(), "theirs changed\n".into()));
            assert_eq!(read(&repo, "bin.dat"), "\0theirs");
            assert!(!exists("ud.txt"), "they deleted it");
        } else {
            assert_eq!((read(&repo, "uu.txt"), read(&repo, "aa.txt"), read(&repo, "ud.txt")), ("ours\n".into(), "ours added\n".into(), "ours changed\n".into()));
            assert_eq!(read(&repo, "bin.dat"), "\0ours");
            assert!(!exists("du.txt"), "we deleted it");
        }
    }
}

#[test]
fn conflict_sides_follow_the_operation() {
    let sb = Sandbox::new();
    let tip = |side: &git::ConflictSide| side.tip.as_ref().map(|c| c.subject.clone()).unwrap_or_default();
    let names = |repo: &Path| {
        let (current, incoming) = git::conflict_sides(repo, "f.txt");
        (current.name.clone(), tip(&current), incoming.name.clone(), tip(&incoming))
    };
    let own = |text: &str| text.to_string();

    // Rebase: current is what we rebase onto, incoming is the commit being replayed.
    let repo = sb.repo("rebase");
    diverge(&repo);
    fails(&repo, Op::Rebase { onto: "other".into(), autostash: false });
    assert_eq!(names(&repo), (own("other"), own("theirs change"), own("main"), own("ours change")));
    let (_, incoming) = git::conflict_sides(&repo, "f.txt");
    assert_eq!(incoming.commits.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["ours change"]);
    op(&repo, Op::Abort(InProgress::Rebase));

    // Cherry-pick and revert: HEAD against the commit named by the operation.
    fails(&repo, Op::CherryPick { sha: sha(&repo, "other"), mainline: false });
    assert_eq!(names(&repo), (own("main"), own("ours change"), own("other"), own("theirs change")));
    op(&repo, Op::Abort(InProgress::CherryPick));
    commit(&repo, "f.txt", "ours again\n", "ours again");
    fails(&repo, Op::Revert { sha: sha(&repo, "HEAD~1"), mainline: false });
    assert_eq!(git::in_progress(&repo), InProgress::Revert);
    assert_eq!(names(&repo).3, "ours change", "revert's incoming side is the commit being reverted");
    op(&repo, Op::Abort(InProgress::Revert));

    // A stash that no longer applies: a conflict with no operation in progress.
    write(&repo, "f.txt", "stashed\n");
    op(&repo, Op::StashPush { message: String::new(), include_untracked: false });
    commit(&repo, "f.txt", "moved on\n", "moved on");
    fails(&repo, Op::StashApply { name: "stash@{0}".into(), pop: true });
    assert_eq!(git::in_progress(&repo), InProgress::None);
    let stuck = conflict_of(&repo, "f.txt");
    assert_eq!((stuck.conflict_reason(), conflict::file_state(&repo, &stuck)), ("both modified", FileState::Text));
    assert_eq!(names(&repo), (own("main"), own("moved on"), own("Stash or patch"), String::new()));

    // The editor still resolves it; there is nothing to continue afterwards.
    let (original, mut doc) = conflict::load(&repo, "f.txt").unwrap();
    doc.conflicts_mut().for_each(|c| c.take(Take::Incoming));
    conflict::save(&repo, "f.txt", &original, &doc.render()).unwrap();
    op(&repo, stage(&["f.txt"]));
    assert_eq!(read(&repo, "f.txt"), "stashed\n");
    assert!(!git::status(&repo).iter().any(Change::conflicted));
    assert_eq!(git::stashes(&repo).len(), 1, "the stash is kept after a conflicting pop");
}

// ---------------------------------------------------------------- batches

#[test]
fn batch_runs_every_operation_and_reports_each_failure() {
    let sb = Sandbox::new();
    let (origin, repo) = sb.with_origin();
    for name in ["merged-1", "merged-2"] {
        op(&repo, branch(name, false));
    }
    op(&repo, branch("unmerged", true));
    commit(&repo, "u.txt", "u\n", "only here");
    op(&repo, checkout("main"));
    let delete = |name: &str| Op::DeleteBranch { name: name.into(), force: false };
    let ops = [delete("merged-1"), delete("unmerged"), delete("merged-2")];
    let locals = |repo: &Path| git::branches(repo).into_iter().filter(|b| b.remote.is_none()).map(|b| b.name).collect::<Vec<_>>();

    // Dependent steps stop at the first failure.
    let stopped = git::run_ops(&repo, &ops, false);
    assert!(!stopped.ok);
    assert!(stopped.error.contains("unmerged") && !stopped.error.contains("failed:"), "{}", stopped.error);
    assert_eq!(locals(&repo), ["main", "merged-2", "unmerged"]);

    // A batch runs them all and names what failed.
    op(&repo, branch("merged-1", false));
    let batch = git::run_ops(&repo, &ops, true);
    assert!(!batch.ok);
    assert!(batch.error.starts_with("1 of 3 failed:\n"), "{}", batch.error);
    assert!(batch.error.contains("unmerged"), "{}", batch.error);
    assert_eq!(batch.log.matches("$ git branch -d").count(), 3);
    assert_eq!(locals(&repo), ["main", "unmerged"], "the unmerged branch must survive a non-forced batch");

    let forced = git::run_ops(&repo, &[Op::DeleteBranch { name: "unmerged".into(), force: true }], true);
    assert!(forced.ok && forced.error.is_empty());
    assert_eq!(locals(&repo), ["main"]);
    assert!(git::run_ops(&repo, &[], true).ok);

    // Tags.
    for name in ["t1", "t2"] {
        op(&repo, Op::CreateTag { name: name.into(), target: "HEAD".into(), message: String::new() });
    }
    let tag = |name: &str| Op::DeleteTag { name: name.into() };
    let tags = git::run_ops(&repo, &[tag("t1"), tag("missing"), tag("t2")], true);
    assert!(tags.error.starts_with("1 of 3 failed:") && tags.error.contains("missing"), "{}", tags.error);
    assert!(git::tags(&repo).is_empty());

    // Remote branches.
    for name in ["x", "y"] {
        op(&repo, branch(name, false));
        op(&repo, push(name, false));
    }
    let remote = |name: &str| Op::DeleteRemoteBranch { remote: "origin".into(), name: name.into() };
    let unreachable = Op::DeleteRemoteBranch { remote: "no-such-remote".into(), name: "x".into() };
    let remotes = git::run_ops(&repo, &[remote("x"), unreachable, remote("y")], true);
    assert!(remotes.error.starts_with("1 of 3 failed:") && remotes.error.contains("no-such-remote"), "{}", remotes.error);
    assert_eq!(g(&origin, &["branch", "--format=%(refname:short)"]), "main");

    // Worktrees: the dirty one survives unless forced.
    let folder = |name: &str| sb.root.join(name).to_str().unwrap().replace('\\', "/");
    for name in ["x", "y"] {
        op(&repo, Op::WorktreeAdd { path: folder(&format!("wt-{name}")), target: name.into(), new_branch: None });
    }
    let paths: Vec<String> = git::worktrees(&repo).into_iter().filter(|t| !t.main).map(|t| t.path).collect();
    write(Path::new(&paths[1]), "unsaved.txt", "precious\n");
    let remove = |force| paths.iter().map(|p| Op::WorktreeRemove { path: p.clone(), force }).collect::<Vec<_>>();
    let kept = git::run_ops(&repo, &remove(false), true);
    assert!(kept.error.starts_with("1 of 2 failed:"), "{}", kept.error);
    assert!(!Path::new(&paths[0]).exists());
    assert_eq!(read(Path::new(&paths[1]), "unsaved.txt"), "precious\n");
    assert!(git::run_ops(&repo, &remove(true)[1..], true).ok);
    assert_eq!(git::worktrees(&repo).len(), 1);
}

#[test]
fn sidebar_selection_toggles_and_extends() {
    use crate::repo::apply_selection;
    let order: Vec<String> = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
    let (mut marked, mut anchor) = (std::collections::BTreeSet::new(), None);
    let click = |key: &str, shift: bool, marked: &mut std::collections::BTreeSet<String>, anchor: &mut Option<String>| {
        apply_selection(marked, anchor, &order, key, shift);
        marked.iter().map(String::as_str).collect::<String>()
    };

    // Ctrl toggles and moves the anchor.
    assert_eq!(click("b", false, &mut marked, &mut anchor), "b");
    assert_eq!(click("d", false, &mut marked, &mut anchor), "bd");
    assert_eq!(click("d", false, &mut marked, &mut anchor), "b");
    assert_eq!(anchor.as_deref(), Some("d"));

    // Shift replaces the selection with the range from the anchor, in either direction.
    assert_eq!(click("a", true, &mut marked, &mut anchor), "abcd");
    assert_eq!(click("e", true, &mut marked, &mut anchor), "de");
    assert_eq!(click("d", true, &mut marked, &mut anchor), "d");
    assert_eq!(anchor.as_deref(), Some("d"), "a range does not move the anchor");

    // Without a usable anchor, Shift behaves like a toggle.
    (marked, anchor) = (Default::default(), None);
    assert_eq!(click("c", true, &mut marked, &mut anchor), "c");
    anchor = Some("filtered-out".into());
    assert_eq!(click("a", true, &mut marked, &mut anchor), "ac");
    assert_eq!(anchor.as_deref(), Some("a"));
}

#[test]
fn branches_group_into_folders_by_prefix() {
    use crate::repo::{TreeRow, tree_rows};
    let names = ["main", "develop", "feat/anims/Value", "feat/anims/Animation", "feat/twin/x", "feat/workstation-setup", "backup/pre-split", "Hotfix-1.2.1", "feat/anims-rebase-a01"];
    // Compact form: depth, then "+name" open folder, "-name" closed folder, or a branch's full name.
    let show = |opened: &[&str], all_open: bool| -> Vec<String> {
        tree_rows(&names, "local", &|key| all_open || opened.contains(&key))
            .iter()
            .map(|row| match row {
                TreeRow::Folder { name, depth, open, .. } => format!("{depth}{}{name}", if *open { '+' } else { '-' }),
                TreeRow::Leaf { index, name, depth } => {
                    assert!(names[*index].ends_with(name), "leaf name is the last part of the branch");
                    format!("{depth} {}", names[*index])
                }
            })
            .collect()
    };

    // Folders first, then branches, alphabetical without regard to case, nested by every '/'.
    let expected = [
        "0+backup", "1 backup/pre-split",
        "0+feat", "1+anims", "2 feat/anims/Animation", "2 feat/anims/Value", "1+twin", "2 feat/twin/x", "1 feat/anims-rebase-a01", "1 feat/workstation-setup",
        "0 develop", "0 Hotfix-1.2.1", "0 main",
    ];
    let all = ["local/backup", "local/feat", "local/feat/anims", "local/feat/twin"];
    assert_eq!(show(&all, false), expected);

    // Folders start closed: a closed folder keeps its row and hides everything under it.
    let closed = ["0-backup", "0-feat", "0 develop", "0 Hotfix-1.2.1", "0 main"];
    assert_eq!(show(&[], false), closed);
    assert_eq!(show(&["local/backup", "local/feat/anims", "local/feat/twin"], false), ["0+backup", "1 backup/pre-split", "0-feat", "0 develop", "0 Hotfix-1.2.1", "0 main"], "an open folder inside a closed one stays hidden");
    let nested = show(&["local/feat", "local/feat/twin"], false);
    assert!(nested.contains(&"1-anims".to_string()) && nested.contains(&"2 feat/twin/x".to_string()) && !nested.iter().any(|r| r.contains("Animation")));
    // Another tree's folders do not open this one; filtering opens everything.
    assert_eq!(show(&["remote/origin/feat"], false), closed);
    assert_eq!(show(&[], true), expected);

    // Keys name the folder's full path, so equal names at different places fold apart.
    let keys: Vec<String> = tree_rows(&names, "remote/origin", &|_| true).into_iter().filter_map(|row| if let TreeRow::Folder { key, .. } = row { Some(key) } else { None }).collect();
    assert_eq!(keys, ["remote/origin/backup", "remote/origin/feat", "remote/origin/feat/anims", "remote/origin/feat/twin"]);

    // Odd names never loop or vanish: every branch still appears exactly once.
    let odd = ["a//b", "/lead", "trail/", "//", "", "x/y/z/w"];
    let mut seen: Vec<usize> = tree_rows(&odd, "local", &|_| true).iter().filter_map(|row| if let TreeRow::Leaf { index, .. } = row { Some(*index) } else { None }).collect();
    seen.sort();
    assert_eq!(seen, [0, 1, 2, 3, 4, 5]);
}

#[test]
fn history_rows_stay_short() {
    use crate::repo::{ago, clip_name, ref_chips};
    // Relative time, in the fewest characters; a plain day once it is over a year old.
    let label = |secs: i64| ago(1_000_000_000 - secs, 1_000_000_000, "2001-09-09 01:46");
    assert_eq!([label(0), label(59), label(60), label(3_599)], ["now", "now", "1 min", "59 min"]);
    assert_eq!([label(3_600), label(86_399), label(86_400), label(13 * 86_400)], ["1 h", "23 h", "1 d", "13 d"]);
    assert_eq!([label(14 * 86_400), label(364 * 86_400), label(365 * 86_400)], ["2 wk", "52 wk", "2001-09-09"]);
    assert_eq!(label(-500), "now", "a clock ahead of ours is not a negative age");

    // The log carries the time those labels are computed from.
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "first");
    assert!(log(&repo)[0].time > 1_600_000_000);

    // Long names keep both ends; short ones and non-ASCII ones are left whole or cut on characters.
    assert_eq!(clip_name("main"), "main");
    assert_eq!(clip_name("fix/Bug-Hunt-Edit-Receipts"), "fix/Bug-H\u{2026}Edit-Receipts");
    assert_eq!(clip_name("\u{17c}\u{f3}\u{142}\u{107}-".repeat(6).as_str()).chars().count(), 23);

    // A remote branch on the same commit as its local one folds into that chip.
    let refs = git::parse_commits("a\0\0HEAD -> refs/heads/main, tag: refs/tags/v1, refs/remotes/origin/main, refs/remotes/origin/other, refs/heads/topic\0n\0e\0d\x001\0s\n").remove(0).refs;
    let chips: Vec<(String, bool)> = ref_chips(&refs).into_iter().map(|(r, synced)| (r.name.clone(), synced)).collect();
    let own = |name: &str, synced: bool| (name.to_string(), synced);
    assert_eq!(chips, [own("main", true), own("topic", false), own("v1", false), own("origin/other", false)]);
}

#[test]
fn open_in_finds_web_pages_and_installed_ides() {
    use crate::open_in::{jetbrains, site_name, visual_studio_name, web_url};
    let page = |remote: &str| web_url(remote).unwrap_or_default();
    // Every way git writes a remote leads to the same page.
    for remote in ["git@github.com:me/repo.git", "https://github.com/me/repo.git", "https://token@github.com/me/repo/", "ssh://git@github.com:22/me/repo", "git://github.com/me/repo.git"] {
        assert_eq!(page(remote), "https://github.com/me/repo", "{remote}");
    }
    assert_eq!(page("http://git.local:3000/team/repo.git"), "http://git.local:3000/team/repo", "a web port is kept");
    assert_eq!(page("git@gitlab.example.com:group/sub/repo.git"), "https://gitlab.example.com/group/sub/repo");
    // Local remotes have no page.
    for remote in [r"C:\work\repo.git", "C:/work/repo", "../repo", "/srv/git/repo.git", "file:///srv/git/repo.git", ""] {
        assert_eq!(web_url(remote), None, "{remote}");
    }
    assert_eq!([site_name("https://github.com/me/repo"), site_name("https://gitlab.com/a/b"), site_name("https://bitbucket.org/a/b")], ["GitHub", "GitLab", "Bitbucket"]);
    assert_eq!(site_name("http://git.local:3000/team/repo"), "git.local:3000");

    // JetBrains Toolbox lists its IDEs in state.json; entries missing a part are skipped.
    let state = r#"{"tools": [
        {"productCode": "RD", "displayName": "Rider", "installLocation": "/opt/rider", "launchCommand": "bin/rider.sh"},
        {"displayName": "Broken", "installLocation": "", "launchCommand": "bin/x"},
        {"displayName": "No launcher", "installLocation": "/opt/y"}
    ]}"#;
    assert_eq!(jetbrains(state), [("Rider".to_string(), PathBuf::from("/opt/rider").join("bin/rider.sh"))]);
    assert!(jetbrains("not json").is_empty() && jetbrains("{}").is_empty() && jetbrains("").is_empty());

    assert_eq!(visual_studio_name(Path::new(r"C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\devenv.exe")), "Visual Studio 2022");
    assert_eq!(visual_studio_name(Path::new(r"C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\devenv.exe")), "Visual Studio");

    // Whatever this machine has, detection finishes and names everything it found.
    assert!(crate::open_in::targets().iter().all(|target| !target.name.is_empty()));
}

#[test]
fn diff_goes_to_an_external_program() {
    use crate::open_in::{DiffTool, editor_diff_args};
    // The command is one a POSIX shell reads: forward slashes, the path in single quotes.
    let tool = DiffTool::new("Code", PathBuf::from(r"C:\Program Files\It's Code\Code.exe"), &["--wait", "--diff"]);
    assert_eq!(tool.command(), r"'C:/Program Files/It'\''s Code/Code.exe' --wait --diff");
    assert_eq!(DiffTool::new("Meld", PathBuf::from("/usr/bin/meld"), &[]).command(), "'/usr/bin/meld'");
    // Which editors can compare files, and how they are told to.
    assert_eq!(editor_diff_args("Cursor"), Some(&["--wait", "--diff"][..]));
    assert_eq!(editor_diff_args("Visual Studio 2022"), Some(&["//diff"][..]));
    assert_eq!(editor_diff_args("Rider"), Some(&["diff"][..]));
    assert_eq!([editor_diff_args("Zed"), editor_diff_args("Sublime Text")], [None, None]);
    assert!(crate::open_in::diff_tools().iter().all(|tool| !tool.command().is_empty()));

    // `cp` stands in for the program: given the old and the new file, it puts the old
    // content back, which shows it was run with the right two files.
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let first = commit(&repo, "f.txt", "old\n", "first");
    write(&repo, "f.txt", "new\n");
    let unstaged = DiffSpec::Unstaged(change(&git::status(&repo), "f.txt").clone());
    let shown = git::difftool(&repo, "cp", &unstaged);
    assert!(shown.ok, "{}", shown.log);
    assert_eq!(read(&repo, "f.txt"), "old\n");

    // Between two commits both sides are temporary copies; the work tree is not touched.
    let second = commit(&repo, "f.txt", "newer\n", "second");
    let revs = DiffSpec::Revs { base: first, rev: second, change: Change { path: "f.txt".into(), index: 'M', ..Default::default() } };
    assert!(git::difftool(&repo, "cp", &revs).ok);
    assert_eq!(read(&repo, "f.txt"), "newer\n");

    // A program that is not there is reported, with what the shell said.
    let missing = git::difftool(&repo, "no-such-diff-program-anywhere", &revs);
    assert!(!missing.ok && missing.error.contains("not found"), "{}", missing.log);
}

// ---------------------------------------------------------------- remotes

#[test]
fn push_fetch_pull_between_clones() {
    let sb = Sandbox::new();
    let (origin, a) = sb.with_origin();
    let b = sb.clone(&origin, "b");
    assert_eq!(subjects(&b), ["first"]);
    assert_eq!(git::remotes(&b), [git::Remote { name: "origin".into(), url: origin.to_str().unwrap().into() }]);

    let main = local(&git::branches(&a), "main").clone();
    assert_eq!((main.upstream.as_str(), main.ahead, main.behind, main.gone), ("origin/main", 0, 0, false));

    // a commits and pushes; b sees it only after fetch, and is then behind.
    commit(&a, "f.txt", "two\n", "second");
    assert_eq!(local(&git::branches(&a), "main").ahead, 1);
    op(&a, push("main", false));
    assert_eq!(local(&git::branches(&a), "main").ahead, 0);

    assert_eq!(local(&git::branches(&b), "main").behind, 0);
    op(&b, fetch());
    assert_eq!(local(&git::branches(&b), "main").behind, 1);
    assert_eq!(subjects(&b)[0], "second", "fetched commit shows up through the remote branch");

    op(&b, pull(false));
    assert_eq!(sha(&b, "HEAD"), sha(&a, "HEAD"));
    assert_eq!(read(&b, "f.txt"), "two\n");
    assert_eq!(local(&git::branches(&b), "main").behind, 0);
}

#[test]
fn rejected_push_then_pull_merge_or_rebase() {
    for rebase in [false, true] {
        let sb = Sandbox::new();
        let (origin, a) = sb.with_origin();
        let b = sb.clone(&origin, "b");
        commit(&a, "a.txt", "a\n", "from a");
        op(&a, push("main", false));
        commit(&b, "b.txt", "b\n", "from b");

        // Non-fast-forward push is rejected and nothing is lost on the remote.
        let error = fails(&b, push("main", false));
        assert!(error.contains("rejected") || error.contains("fetch first"), "{error}");
        assert_eq!(sha(&origin, "main"), sha(&a, "HEAD"));

        op(&b, pull(rebase));
        let commits = log(&b);
        if rebase {
            assert_eq!(subjects(&b), ["from b", "from a", "first"]);
            assert!(commits.iter().all(|c| c.parents.len() <= 1));
        } else {
            assert_eq!(commits[0].parents.len(), 2, "pull without rebase creates a merge commit");
        }
        assert!(b.join("a.txt").exists() && b.join("b.txt").exists());

        op(&b, push("main", false));
        assert_eq!(sha(&origin, "main"), sha(&b, "HEAD"));
    }
}

#[test]
fn pull_conflict_can_be_aborted() {
    let sb = Sandbox::new();
    let (origin, a) = sb.with_origin();
    let b = sb.clone(&origin, "b");
    commit(&a, "f.txt", "from a\n", "a edit");
    op(&a, push("main", false));
    let before = commit(&b, "f.txt", "from b\n", "b edit");

    fails(&b, pull(false));
    assert_eq!(git::in_progress(&b), InProgress::Merge);
    op(&b, Op::Abort(InProgress::Merge));
    assert_eq!((sha(&b, "HEAD"), read(&b, "f.txt")), (before.clone(), "from b\n".to_string()));

    fails(&b, pull(true));
    assert_eq!(git::in_progress(&b), InProgress::Rebase);
    op(&b, Op::Abort(InProgress::Rebase));
    assert_eq!((sha(&b, "HEAD"), read(&b, "f.txt")), (before, "from b\n".to_string()));
}

#[test]
fn force_push_uses_lease() {
    let sb = Sandbox::new();
    let (origin, a) = sb.with_origin();
    let b = sb.clone(&origin, "b");
    let force = Op::Push { remote: "origin".into(), local: "main".into(), remote_branch: "main".into(), force: true, set_upstream: false, tags: false };

    // Rewritten history needs force; with an up-to-date lease it goes through.
    op(&a, Op::Commit { message: "first, reworded".into(), amend: true, signoff: false });
    fails(&a, push("main", false));
    op(&a, force.clone());
    assert_eq!(sha(&origin, "main"), sha(&a, "HEAD"));

    // b still has the stale remote-tracking ref: the lease must protect a's push.
    op(&b, Op::Commit { message: "b rewrites too".into(), amend: true, signoff: false });
    let error = fails(&b, force);
    assert!(error.contains("stale info") || error.contains("rejected"), "{error}");
    assert_eq!(sha(&origin, "main"), sha(&a, "HEAD"), "force-with-lease must not overwrite unseen work");
}

#[test]
fn remote_branches_track_checkout_delete_and_prune() {
    let sb = Sandbox::new();
    let (origin, a) = sb.with_origin();
    let b = sb.clone(&origin, "b");

    op(&a, branch("feature/x", true));
    commit(&a, "x.txt", "x\n", "x work");
    op(&a, push("feature/x", true));
    assert_eq!(local(&git::branches(&a), "feature/x").upstream, "origin/feature/x");

    op(&b, fetch());
    let remote = git::branches(&b).into_iter().find(|br| br.name == "origin/feature/x").expect("remote branch listed");
    assert_eq!(remote.remote.as_deref(), Some("origin"));
    assert!(!git::branches(&b).iter().any(|br| br.name.ends_with("/HEAD")));

    op(&b, Op::CheckoutRemote { remote_branch: "origin/feature/x".into(), local: "feature/x".into() });
    assert_eq!(current(&b), "feature/x");
    assert_eq!(local(&git::branches(&b), "feature/x").upstream, "origin/feature/x");
    assert_eq!(read(&b, "x.txt"), "x\n");

    // a deletes the remote branch; b learns about it on fetch --prune.
    op(&a, Op::DeleteRemoteBranch { remote: "origin".into(), name: "feature/x".into() });
    assert!(!git::run(&origin, &["rev-parse", "--verify", "-q", "refs/heads/feature/x"]).ok);
    assert!(!branch_names(&a).contains(&"origin/feature/x".to_string()));
    op(&b, fetch());
    assert!(!branch_names(&b).contains(&"origin/feature/x".to_string()));
    assert!(local(&git::branches(&b), "feature/x").gone, "upstream must be reported as gone");
}

#[test]
fn tags_can_be_pushed_and_deleted_remotely() {
    let sb = Sandbox::new();
    let (origin, a) = sb.with_origin();
    op(&a, Op::CreateTag { name: "v1.0".into(), target: "HEAD".into(), message: "one".into() });
    op(&a, Op::PushTag { remote: "origin".into(), tag: "v1.0".into(), delete: false });
    assert_eq!(g(&origin, &["tag", "-l"]), "v1.0");

    let b = sb.clone(&origin, "b");
    assert_eq!(git::tags(&b).len(), 1);

    op(&a, Op::PushTag { remote: "origin".into(), tag: "v1.0".into(), delete: true });
    assert_eq!(g(&origin, &["tag", "-l"]), "");

    // --tags pushes every local tag along with the branch.
    op(&a, Op::CreateTag { name: "v2.0".into(), target: "HEAD".into(), message: String::new() });
    op(&a, Op::Push { remote: "origin".into(), local: "main".into(), remote_branch: "main".into(), force: false, set_upstream: false, tags: true });
    assert_eq!(g(&origin, &["tag", "-l"]), "v1.0\nv2.0");
}

#[test]
fn remotes_add_remove_and_named_fetch() {
    let sb = Sandbox::new();
    let (origin, _a) = sb.with_origin();
    let repo = sb.repo("standalone");
    commit(&repo, "s.txt", "s\n", "standalone");
    assert!(git::remotes(&repo).is_empty());

    op(&repo, Op::AddRemote { name: "up".into(), url: origin.to_str().unwrap().into() });
    assert_eq!(git::remotes(&repo)[0].name, "up");
    fails(&repo, Op::AddRemote { name: "up".into(), url: "elsewhere".into() });

    op(&repo, Op::Fetch { remote: Some("up".into()), prune: false });
    assert!(branch_names(&repo).contains(&"up/main".to_string()));
    fails(&repo, Op::Fetch { remote: Some("nope".into()), prune: false });

    op(&repo, Op::RemoveRemote { name: "up".into() });
    assert!(git::remotes(&repo).is_empty());
    assert_eq!(branch_names(&repo), ["main"]);
}

// ---------------------------------------------------------------- safety

#[test]
fn option_like_names_are_rejected_before_git_runs() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "first");
    let evil = || "--upload-pack=touch pwned".to_string();
    let ok = || "main".to_string();
    let ops = vec![
        Op::Fetch { remote: Some(evil()), prune: false },
        Op::Pull { remote: evil(), branch: ok(), rebase: false, autostash: false },
        Op::Pull { remote: ok(), branch: evil(), rebase: false, autostash: false },
        Op::Push { remote: evil(), local: ok(), remote_branch: ok(), force: false, set_upstream: false, tags: false },
        Op::Push { remote: ok(), local: evil(), remote_branch: ok(), force: false, set_upstream: false, tags: false },
        Op::PushTag { remote: evil(), tag: ok(), delete: false },
        Op::Checkout { target: evil() },
        Op::CheckoutRemote { remote_branch: evil(), local: ok() },
        Op::CheckoutRemote { remote_branch: ok(), local: evil() },
        Op::CreateBranch { name: evil(), base: ok(), checkout: false },
        Op::CreateBranch { name: ok(), base: evil(), checkout: true },
        Op::DeleteBranch { name: evil(), force: true },
        Op::DeleteRemoteBranch { remote: evil(), name: ok() },
        Op::RenameBranch { old: ok(), new: evil() },
        Op::Merge { source: evil(), mode: MergeMode::Default },
        Op::Rebase { onto: evil(), autostash: false },
        Op::CherryPick { sha: evil(), mainline: false },
        Op::Revert { sha: evil(), mainline: false },
        Op::Reset { target: evil(), mode: ResetMode::Hard },
        Op::CreateTag { name: evil(), target: ok(), message: String::new() },
        Op::CreateTag { name: ok(), target: evil(), message: String::new() },
        Op::DeleteTag { name: evil() },
        Op::StashApply { name: evil(), pop: true },
        Op::StashDrop { name: evil() },
        Op::AddRemote { name: evil(), url: ok() },
        Op::AddRemote { name: ok(), url: evil() },
        Op::RemoveRemote { name: evil() },
        Op::WorktreeAdd { path: evil(), target: ok(), new_branch: None },
        Op::WorktreeAdd { path: ok(), target: evil(), new_branch: None },
        Op::WorktreeAdd { path: ok(), target: ok(), new_branch: Some(evil()) },
        Op::WorktreeRemove { path: evil(), force: true },
        Op::Checkout { target: "  ".into() },
        Op::Checkout { target: "a\nb".into() },
    ];
    let before = (sha(&repo, "HEAD"), git::branches(&repo), git::tags(&repo), git::remotes(&repo));
    for bad in ops {
        let error = bad.steps().expect_err(&format!("{bad:?} must be rejected"));
        assert!(error.contains("must not start") || error.contains("empty") || error.contains("control"), "{error}");
        assert!(!git::run_op(&repo, &bad).ok);
    }
    assert_eq!(before, (sha(&repo, "HEAD"), git::branches(&repo), git::tags(&repo), git::remotes(&repo)));
    assert!(!git::clone("--upload-pack=x", &sb.root, "c").ok);
    assert!(!git::clone("somewhere", &sb.root, "-c").ok);
    assert_eq!(Op::Continue(InProgress::None).steps(), Err("no operation in progress".into()));
}

#[test]
fn failures_report_the_git_error() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "a\n", "first");
    let result = git::run_op(&repo, &merge("no-such-branch", MergeMode::Default));
    assert!(!result.ok);
    assert!(result.error.contains("no-such-branch"), "{}", result.error);
    assert!(result.log.starts_with("$ git merge --no-edit --ff no-such-branch\n"), "{}", result.log);

    let not_a_repo = sb.root.join("plain");
    fs::create_dir_all(&not_a_repo).unwrap();
    assert_eq!(git::root(&not_a_repo), None);
    assert!(!git::run_op(&not_a_repo, &Op::StageAll).ok);
    assert!(!git::run(&sb.root.join("missing-dir"), &["status"]).ok);
}

// ---------------------------------------------------------------- queries

#[test]
fn init_clone_root_and_snapshot() {
    let sb = Sandbox::new();
    let (origin, a) = sb.with_origin();
    write(&a, "sub/dir/x.txt", "x\n");
    assert_eq!(fs::canonicalize(git::root(&a.join("sub/dir")).unwrap()).unwrap(), fs::canonicalize(&a).unwrap());

    op(&a, Op::CreateTag { name: "t".into(), target: "HEAD".into(), message: String::new() });
    op(&a, Op::StashPush { message: "s".into(), include_untracked: true });
    write(&a, "dirty.txt", "d\n");
    let snap = git::snapshot(&a, &LogQuery::default());
    assert_eq!(snap.head, Head::Branch("main".into()));
    assert_eq!(snap.state, InProgress::None);
    assert_eq!((snap.commits.len(), snap.tags.len(), snap.stashes.len(), snap.remotes.len(), snap.changes.len()), (1, 1, 1, 1, 1));
    assert_eq!(snap.branches.len(), 2);
    assert_eq!(snap.worktrees.len(), 1);
    assert_eq!(snap.worktrees[0].branch, "main");
    assert!(snap.submodules.is_empty());

    // A brand-new repository has no commits and an unborn branch.
    let fresh = sb.repo("fresh");
    let snap = git::snapshot(&fresh, &LogQuery::default());
    assert_eq!(snap.head, Head::Branch("main".into()));
    assert!(snap.commits.is_empty() && snap.branches.is_empty());
    drop(origin);
}

#[test]
fn log_decorations_search_and_limits() {
    let sb = Sandbox::new();
    let (_origin, repo) = sb.with_origin();
    g(&repo, &["config", "user.name", "Second Author"]);
    commit(&repo, "src/lib.rs", "fn needle() {}\n", "add needle function");
    g(&repo, &["config", "user.name", "Spoon Test"]);
    op(&repo, branch("side", true));
    commit(&repo, "side.txt", "s\n", "side only");
    op(&repo, checkout("main"));
    commit(&repo, "docs.md", "docs\n", "write docs");

    let commits = log(&repo);
    assert_eq!(commits.len(), 4);
    let top = commits.iter().find(|c| c.is_head).unwrap();
    assert_eq!(top.subject, "write docs");
    assert_eq!(top.refs, [git::Ref { kind: RefKind::Head, name: "main".into() }]);
    assert_eq!(top.parents.len(), 1);
    assert_eq!((top.author.as_str(), top.email.as_str()), ("Spoon Test", "test@spoon.invalid"));
    assert_eq!(top.date.len(), "2026-01-01 00:00".len(), "{}", top.date);
    let first = commits.last().unwrap();
    assert!(first.parents.is_empty());
    assert!(first.refs.contains(&git::Ref { kind: RefKind::Remote, name: "origin/main".into() }));
    assert!(commits.iter().any(|c| c.refs.contains(&git::Ref { kind: RefKind::Local, name: "side".into() })));
    assert_eq!(commits.iter().filter(|c| c.is_head).count(), 1);

    let search = |by, text: &str, all| {
        let q = LogQuery { limit: 100, all_branches: all, search: Some((by, text.to_string())) };
        git::commits(&repo, &q).into_iter().map(|c| c.subject).collect::<Vec<_>>()
    };
    assert_eq!(search(SearchBy::Message, "NEEDLE add", true), ["add needle function"]);
    assert_eq!(search(SearchBy::Message, "needle missing", true), Vec::<String>::new());
    assert_eq!(search(SearchBy::Author, "second", true), ["add needle function"]);
    assert_eq!(search(SearchBy::Path, "src/lib.rs", true), ["add needle function"]);
    assert_eq!(search(SearchBy::Content, "needle\\(\\)", true), ["add needle function"]);
    assert_eq!(search(SearchBy::Message, "side", true), ["side only"]);
    assert_eq!(search(SearchBy::Message, "side", false), Vec::<String>::new(), "current-branch search must not see other branches");
    assert_eq!(search(SearchBy::Path, "--all", true), Vec::<String>::new(), "path is never parsed as an option");

    let limited = git::commits(&repo, &LogQuery { limit: 2, all_branches: true, search: None });
    assert_eq!(limited.len(), 2);
    let own = git::commits(&repo, &LogQuery { limit: 100, all_branches: false, search: None });
    assert!(!own.iter().any(|c| c.subject == "side only"));
}

#[test]
fn diffs_for_every_source() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    let root = commit(&repo, "a.txt", "one\ntwo\nthree\n", "root");
    write(&repo, "a.txt", "one\n2\nthree\nfour\n");
    write(&repo, "new.txt", "hello\nworld\n");
    write(&repo, "bin.dat", "\0\u{1}\0\u{2}");

    let changes = git::status(&repo);
    let unstaged = diff::parse(&git::diff_text(&repo, &DiffSpec::Unstaged(change(&changes, "a.txt").clone())));
    assert_eq!((unstaged.added, unstaged.deleted, unstaged.binary), (2, 1, false));

    let untracked = diff::parse(&git::diff_text(&repo, &DiffSpec::Unstaged(change(&changes, "new.txt").clone())));
    assert_eq!((untracked.added, untracked.deleted), (2, 0));
    assert_eq!(untracked.lines[1].text, "hello");

    let binary = diff::parse(&git::diff_text(&repo, &DiffSpec::Unstaged(change(&changes, "bin.dat").clone())));
    assert!(binary.binary && binary.lines.is_empty());

    op(&repo, stage(&["a.txt"]));
    let changes = git::status(&repo);
    let staged = diff::parse(&git::diff_text(&repo, &DiffSpec::Staged(change(&changes, "a.txt").clone())));
    assert_eq!((staged.added, staged.deleted), (2, 1));
    let nothing = git::diff_text(&repo, &DiffSpec::Unstaged(change(&changes, "a.txt").clone()));
    assert!(nothing.is_empty(), "fully staged file has no unstaged diff");

    op(&repo, commit_op("second"));
    g(&repo, &["mv", "a.txt", "moved.txt"]);
    op(&repo, commit_op("rename"));

    let commits = log(&repo);
    let files = git::changed_files(&repo, &git::base_of(&commits[0]), &commits[0].sha);
    assert_eq!(files, [Change { path: "moved.txt".into(), orig: Some("a.txt".into()), index: 'R', work: ' ' }]);
    let rename = git::diff_text(&repo, &DiffSpec::Revs { base: git::base_of(&commits[0]), rev: commits[0].sha.clone(), change: files[0].clone() });
    assert!(rename.contains("rename from a.txt") && diff::parse(&rename).lines.is_empty());

    // Root commit diffs against the empty tree.
    let root_commit = commits.last().unwrap();
    assert_eq!((root_commit.sha.as_str(), git::base_of(root_commit).as_str()), (root.as_str(), git::EMPTY_TREE));
    let files = git::changed_files(&repo, git::EMPTY_TREE, &root);
    assert_eq!(files, [Change { path: "a.txt".into(), orig: None, index: 'A', work: ' ' }]);
    let text = git::diff_text(&repo, &DiffSpec::Revs { base: git::EMPTY_TREE.into(), rev: root, change: files[0].clone() });
    assert_eq!(diff::parse(&text).added, 3);

    assert!(git::blame(&repo, "", "moved.txt").contains("Spoon Test"));
}

#[test]
fn diff_parser_numbers_lines() {
    let raw = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -10,4 +10,5 @@ fn ctx()\n keep\n-old\n+new\n+\tadded\n keep2\n\\ No newline at end of file\n@@ -40 +41,2 @@\n-x\n+y\n+z\n";
    let d = diff::parse(raw);
    use diff::LineKind::*;
    let got: Vec<_> = d.lines.iter().map(|l| (l.kind, l.old, l.new, l.text.as_str())).collect();
    assert_eq!(
        got,
        [
            (Hunk, None, None, "@@ -10,4 +10,5 @@ fn ctx()"),
            (Context, Some(10), Some(10), "keep"),
            (Deleted, Some(11), None, "old"),
            (Added, None, Some(11), "new"),
            (Added, None, Some(12), "    added"),
            (Context, Some(12), Some(13), "keep2"),
            (Note, None, None, " No newline at end of file"),
            (Hunk, None, None, "@@ -40 +41,2 @@"),
            (Deleted, Some(40), None, "x"),
            (Added, None, Some(41), "y"),
            (Added, None, Some(42), "z"),
        ]
    );
    assert_eq!((d.added, d.deleted, d.max_chars), (4, 2, 26));
    assert!(diff::parse("").lines.is_empty());
    assert!(diff::parse("Binary files a/x and b/x differ\n").binary);
}

#[test]
fn parsers_handle_edge_cases() {
    let refs = git::parse_refs("HEAD -> refs/heads/main, tag: refs/tags/v1, refs/remotes/origin/main, refs/remotes/origin/HEAD, refs/stash, refs/heads/a, b");
    let kinds: Vec<_> = refs.iter().map(|r| (r.kind.clone(), r.name.as_str())).collect();
    assert_eq!(kinds, [(RefKind::Head, "main"), (RefKind::Tag, "v1"), (RefKind::Remote, "origin/main"), (RefKind::Local, "a")]);
    assert_eq!(git::parse_refs("HEAD")[0], git::Ref { kind: RefKind::Head, name: "HEAD".into() });
    assert!(git::parse_refs("").is_empty());

    let branches = git::parse_branches(
        "refs/heads/main\0abc\0*\0origin/main\0ahead 3, behind 12\n\
         refs/heads/old\0def\0 \0origin/old\0gone\n\
         (HEAD detached at abc)\0abc\0*\0\0\n\
         refs/remotes/origin/HEAD\0abc\0 \0\0\n\
         refs/remotes/origin/feature/deep/x\0abc\0 \0\0\n\
         garbage\n",
    );
    assert_eq!(branches.len(), 3);
    assert_eq!((branches[0].ahead, branches[0].behind, branches[0].current, branches[0].gone), (3, 12, true, false));
    assert!(branches[1].gone && !branches[1].current);
    assert_eq!((branches[2].name.as_str(), branches[2].remote.as_deref()), ("origin/feature/deep/x", Some("origin")));

    let status = git::parse_status("R  new name.txt\0old name.txt\0 M plain.txt\0?? a b/c.txt\0UU both.txt\0AA x\0\0junk\0");
    assert_eq!(status.len(), 5);
    assert_eq!(status[0], Change { path: "new name.txt".into(), orig: Some("old name.txt".into()), index: 'R', work: ' ' });
    assert!(status[2].untracked());
    assert!(status[3].conflicted() && status[4].conflicted());

    let files = git::parse_name_status("M\0a.txt\0R087\0old.txt\0new.txt\0D\0gone.txt\0");
    assert_eq!(files.len(), 3);
    assert_eq!(files[1], Change { path: "new.txt".into(), orig: Some("old.txt".into()), index: 'R', work: ' ' });
    assert!(git::parse_name_status("").is_empty());
    assert!(git::parse_commits("too\0few\0fields\n").is_empty());

    let trees = git::parse_worktrees(
        "worktree /a\nHEAD abc\nbranch refs/heads/main\n\nworktree /b\nHEAD def\ndetached\n\nworktree /c\nHEAD abc\nbranch refs/heads/x/y\nprunable gitdir file points to non-existent location\n",
    );
    let tree = |path: &str, branch: &str, main, prunable| git::Worktree { path: path.into(), branch: branch.into(), main, prunable };
    assert_eq!(trees, [tree("/a", "main", true, false), tree("/b", "detached", false, false), tree("/c", "x/y", false, true)]);
    assert_eq!(git::short("0123456789abcdef"), "0123456789");
    assert_eq!(git::short("abc"), "abc");
}

// ---------------------------------------------------------------- graph

fn near(p: graph::Point, q: graph::Point) -> bool {
    (p.0 - q.0).abs() < 0.01 && (p.1 - q.1).abs() < 0.01
}

/// Lanes only store bend points, so a dot may sit on a vertical run between two of them.
fn on_lane(lane: &graph::Lane, q: graph::Point) -> bool {
    let vertical = |w: &[graph::Point]| (w[0].0 - q.0).abs() < 0.01 && (w[1].0 - q.0).abs() < 0.01 && w[0].1 <= q.1 && q.1 <= w[1].1;
    lane.points.iter().any(|p| near(*p, q)) || lane.points.windows(2).any(vertical)
}

fn assert_graph_sane(commits: &[git::Commit], g: &graph::Graph) {
    assert_eq!(g.dots.len(), commits.len());
    assert_eq!(g.margins.len(), commits.len());
    for (i, dot) in g.dots.iter().enumerate() {
        assert_eq!(dot.center.1, i as f32 + 0.5, "dot {i} sits in its own row");
        assert!(dot.center.0 > 0.0 && dot.center.0 < g.margins[i], "dot {i} left of the row text");
        assert!(dot.color < graph::LANE_COLORS);
    }
    for lane in &g.lanes {
        assert!(lane.points.len() >= 2, "lane with a single point: {lane:?}");
        assert!(lane.points.windows(2).all(|w| w[0].1 <= w[1].1), "lane goes upwards: {lane:?}");
        assert!(lane.points.iter().all(|p| p.0 > 0.0));
    }
    // Every parent edge is drawn: a lane or link leaves the child, and reaches the parent's dot.
    for (i, commit) in commits.iter().enumerate() {
        let child = g.dots[i].center;
        for parent in &commit.parents {
            let Some(j) = commits.iter().position(|c| &c.sha == parent) else { continue };
            let target = g.dots[j].center;
            let via_lane = g.lanes.iter().any(|l| on_lane(l, child) && on_lane(l, target));
            let via_link = g.links.iter().any(|k| near(k.start, child) && g.lanes.iter().any(|l| on_lane(l, k.end) && on_lane(l, target)));
            assert!(via_lane || via_link, "edge {} -> {} is not drawn", commit.subject, commits[j].subject);
        }
    }
}

#[test]
fn graph_layout_linear_and_merged_history() {
    let sb = Sandbox::new();
    let repo = sb.repo("r");
    commit(&repo, "a.txt", "1\n", "c1");
    commit(&repo, "a.txt", "2\n", "c2");
    commit(&repo, "a.txt", "3\n", "c3");
    let commits = log(&repo);
    let g = graph::layout(&commits);
    assert_graph_sane(&commits, &g);
    assert_eq!(g.lanes.len(), 1);
    assert!(g.links.is_empty());
    assert!(g.dots.iter().all(|d| d.center.0 == g.dots[0].center.0), "linear history is one straight lane");
    assert_eq!(g.dots[0].kind, graph::DotKind::Head);

    // Two feature branches merged back, one still open.
    op(&repo, branch("f1", true));
    commit(&repo, "f1.txt", "1\n", "f1 a");
    commit(&repo, "f1.txt", "2\n", "f1 b");
    op(&repo, checkout("main"));
    commit(&repo, "m.txt", "1\n", "main a");
    op(&repo, branch("f2", true));
    commit(&repo, "f2.txt", "1\n", "f2 a");
    op(&repo, checkout("main"));
    op(&repo, merge("f1", MergeMode::NoFf));
    commit(&repo, "m.txt", "2\n", "main b");
    op(&repo, merge("f2", MergeMode::NoFf));
    op(&repo, branch("open", true));
    commit(&repo, "o.txt", "1\n", "open work");
    op(&repo, checkout("main"));
    commit(&repo, "m.txt", "3\n", "main c");

    let commits = log(&repo);
    let g = graph::layout(&commits);
    assert_graph_sane(&commits, &g);
    assert!(g.lanes.len() >= 3, "{} lanes", g.lanes.len());
    let merges = commits.iter().filter(|c| c.parents.len() == 2).count();
    assert_eq!(merges, 2);
    assert_eq!(g.dots.iter().filter(|d| d.kind == graph::DotKind::Merge).count(), merges);
    assert_eq!(g.dots.iter().filter(|d| d.kind == graph::DotKind::Head).count(), 1);
    let xs: std::collections::BTreeSet<i32> = g.dots.iter().map(|d| d.center.0 as i32).collect();
    assert!(xs.len() >= 2, "parallel branches use separate lanes");

    // History cut off by the limit still lays out.
    let cut = git::commits(&repo, &LogQuery { limit: 4, all_branches: true, search: None });
    assert_graph_sane(&cut, &graph::layout(&cut));
    assert!(graph::layout(&[]).dots.is_empty());
}
