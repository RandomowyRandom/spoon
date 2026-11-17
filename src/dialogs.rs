//! Operation dialogs as data: a few fields plus a closure that turns them into `Op`s.

use eframe::egui;

use crate::git::{Branch, Change, Commit, Head, MergeMode, Op, ResetMode, Snapshot, short};
use crate::widgets::{self as w, Kind, ic};

pub enum Field {
    Text { label: &'static str, value: String, hint: &'static str, required: bool },
    Toggle { label: &'static str, on: bool },
    Choice { label: &'static str, options: Vec<&'static str>, index: usize },
}

fn text(fields: &[Field], i: usize) -> String {
    match fields.get(i) {
        Some(Field::Text { value, .. }) => value.trim().to_string(),
        _ => String::new(),
    }
}

fn on(fields: &[Field], i: usize) -> bool {
    matches!(fields.get(i), Some(Field::Toggle { on: true, .. }))
}

fn choice(fields: &[Field], i: usize) -> usize {
    match fields.get(i) {
        Some(Field::Choice { index, .. }) => *index,
        _ => 0,
    }
}

fn required(label: &'static str, value: &str, hint: &'static str) -> Field {
    Field::Text { label, value: value.into(), hint, required: true }
}

fn toggle(label: &'static str, on: bool) -> Field {
    Field::Toggle { label, on }
}

pub struct Dialog {
    icon: &'static str,
    title: &'static str,
    body: String,
    fields: Vec<Field>,
    confirm: &'static str,
    danger: bool,
    make: Box<dyn Fn(&[Field]) -> Vec<Op>>,
    focused: bool,
}

pub enum Outcome {
    Open,
    Cancelled,
    Confirmed(Vec<Op>),
}

impl Dialog {
    fn new(icon: &'static str, title: &'static str, body: String, fields: Vec<Field>, confirm: &'static str, make: impl Fn(&[Field]) -> Vec<Op> + 'static) -> Self {
        Self { icon, title, body, fields, confirm, danger: false, make: Box::new(make), focused: false }
    }

    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        let Dialog { icon, title, body, fields, confirm, danger, make, focused } = self;
        let (action, dismissed) = w::dialog(ctx, "operation-dialog", icon, title, 440.0, |ui| {
            if !body.is_empty() {
                w::body_text(ui, body);
            }
            let mut valid = true;
            for field in fields.iter_mut() {
                match field {
                    Field::Text { label, value, hint, required } => {
                        w::field_label(ui, label);
                        let width = ui.available_width();
                        let resp = w::text_field(ui, value, hint, None, width);
                        if !*focused {
                            resp.request_focus();
                            *focused = true;
                        }
                        valid &= !*required || !value.trim().is_empty();
                    }
                    Field::Toggle { label, on } => {
                        ui.add_space(14.0);
                        w::switch(ui, on, label);
                    }
                    Field::Choice { label, options, index } => {
                        w::field_label(ui, label);
                        let items: Vec<(usize, Option<&str>, &str)> = options.iter().enumerate().map(|(i, o)| (i, None, *o)).collect();
                        w::segmented(ui, label, index, &items, 36.0);
                    }
                }
            }
            let enter = valid && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let kind = if *danger { Kind::Danger } else { Kind::Filled };
            w::dialog_actions(ui, confirm, kind, valid).or(enter.then_some(true))
        });
        match action {
            Some(true) => Outcome::Confirmed(make(fields)),
            Some(false) => Outcome::Cancelled,
            None if dismissed => Outcome::Cancelled,
            None => Outcome::Open,
        }
    }
}

// ---------------------------------------------------------------- constructors

fn current_branch(snap: &Snapshot) -> Result<String, String> {
    match &snap.head {
        Head::Branch(name) => Ok(name.clone()),
        Head::Detached(_) => Err("HEAD is detached: check out a branch first".into()),
    }
}

/// (local branch, remote, remote branch, has upstream) for the current branch.
fn tracking(snap: &Snapshot) -> Result<(String, String, String, bool), String> {
    let local = current_branch(snap)?;
    let upstream = snap.branches.iter().find(|b| b.remote.is_none() && b.name == local).map(|b| b.upstream.as_str()).unwrap_or("");
    for remote in &snap.remotes {
        if let Some(branch) = upstream.strip_prefix(&format!("{}/", remote.name)) {
            return Ok((local, remote.name.clone(), branch.into(), true));
        }
    }
    let remote = snap.remotes.first().ok_or("this repository has no remotes; add one first")?;
    Ok((local.clone(), remote.name.clone(), local, false))
}

pub fn confirm(icon: &'static str, title: &'static str, body: String, confirm: &'static str, ops: Vec<Op>) -> Dialog {
    Dialog::new(icon, title, body, vec![], confirm, move |_| ops.clone())
}

pub fn pull(snap: &Snapshot) -> Result<Dialog, String> {
    let (local, remote, branch, _) = tracking(snap)?;
    let body = format!("Pull {remote}/{branch} into {local}.");
    let fields = vec![toggle("Rebase instead of merge", false), toggle("Stash local changes first", true)];
    Ok(Dialog::new(ic::PULL, "Pull", body, fields, "Pull", move |f| {
        vec![Op::Pull { remote: remote.clone(), branch: branch.clone(), rebase: on(f, 0), autostash: on(f, 1) }]
    }))
}

pub fn push(snap: &Snapshot) -> Result<Dialog, String> {
    let (local, remote, branch, has_upstream) = tracking(snap)?;
    let note = if has_upstream { "" } else { " and track it" };
    let body = format!("Push {local} to {remote}/{branch}{note}.");
    let fields = vec![toggle("Force with lease", false), toggle("Push all tags", false)];
    Ok(Dialog::new(ic::PUSH, "Push", body, fields, "Push", move |f| {
        vec![Op::Push {
            remote: remote.clone(),
            local: local.clone(),
            remote_branch: branch.clone(),
            force: on(f, 0),
            set_upstream: !has_upstream,
            tags: on(f, 1),
        }]
    }))
}

/// Plain push of the current branch, for "Commit & push".
pub fn push_op(snap: &Snapshot) -> Result<Op, String> {
    let (local, remote, remote_branch, has_upstream) = tracking(snap)?;
    Ok(Op::Push { remote, local, remote_branch, force: false, set_upstream: !has_upstream, tags: false })
}

pub fn create_branch(base: String, label: &str) -> Dialog {
    let fields = vec![required("Branch name", "", "feature/my-branch"), toggle("Check out the new branch", true)];
    Dialog::new(ic::BRANCH, "Create branch", format!("Based on {label}."), fields, "Create", move |f| {
        vec![Op::CreateBranch { name: text(f, 0), base: base.clone(), checkout: on(f, 1) }]
    })
}

pub fn rename_branch(old: String) -> Dialog {
    let fields = vec![required("New name", &old, "")];
    Dialog::new(ic::EDIT, "Rename branch", format!("Rename {old}."), fields, "Rename", move |f| {
        vec![Op::RenameBranch { old: old.clone(), new: text(f, 0) }]
    })
}

pub fn delete_branch(branch: &Branch) -> Dialog {
    let name = branch.name.clone();
    if let Some(remote) = branch.remote.clone() {
        let short_name = name.strip_prefix(&format!("{remote}/")).unwrap_or(&name).to_string();
        let body = format!("Delete {short_name} on {remote}. Everyone using that remote loses the branch.");
        let op = Op::DeleteRemoteBranch { remote, name: short_name };
        return confirm(ic::DELETE, "Delete remote branch", body, "Delete", vec![op]).danger();
    }
    let body = format!("Delete local branch {name}.");
    let fields = vec![toggle("Force: delete even if not merged", false)];
    Dialog::new(ic::DELETE, "Delete branch", body, fields, "Delete", move |f| vec![Op::DeleteBranch { name: name.clone(), force: on(f, 0) }]).danger()
}

pub fn checkout_remote(remote_branch: String, remote: &str) -> Dialog {
    let local = remote_branch.strip_prefix(&format!("{remote}/")).unwrap_or(&remote_branch).to_string();
    let fields = vec![required("Local branch name", &local, "")];
    let body = format!("Create a local branch tracking {remote_branch}.");
    Dialog::new(ic::BRANCH, "Check out remote branch", body, fields, "Check out", move |f| {
        vec![Op::CheckoutRemote { remote_branch: remote_branch.clone(), local: text(f, 0) }]
    })
}

pub fn merge(snap: &Snapshot, source: String) -> Result<Dialog, String> {
    let into = current_branch(snap)?;
    let options = vec!["Default", "No fast-forward", "Fast-forward only", "Squash"];
    let fields = vec![Field::Choice { label: "Strategy", options, index: 0 }];
    Ok(Dialog::new(ic::MERGE, "Merge", format!("Merge {source} into {into}."), fields, "Merge", move |f| {
        let mode = [MergeMode::Default, MergeMode::NoFf, MergeMode::FfOnly, MergeMode::Squash][choice(f, 0).min(3)];
        vec![Op::Merge { source: source.clone(), mode }]
    }))
}

pub fn rebase(snap: &Snapshot, onto: String) -> Result<Dialog, String> {
    let branch = current_branch(snap)?;
    let fields = vec![toggle("Stash local changes first", true)];
    Ok(Dialog::new(ic::REBASE, "Rebase", format!("Replay {branch} on top of {onto}."), fields, "Rebase", move |f| {
        vec![Op::Rebase { onto: onto.clone(), autostash: on(f, 0) }]
    }))
}

pub fn reset(snap: &Snapshot, commit: &Commit) -> Result<Dialog, String> {
    let branch = current_branch(snap)?;
    let target = commit.sha.clone();
    let body = format!("Move {branch} to {} \u{201c}{}\u{201d}. Hard also throws away all uncommitted changes.", short(&target), commit.subject);
    let fields = vec![Field::Choice { label: "Keep changes", options: vec!["Soft: staged", "Mixed: unstaged", "Hard: discard"], index: 1 }];
    Ok(Dialog::new(ic::RESET, "Reset branch", body, fields, "Reset", move |f| {
        let mode = [ResetMode::Soft, ResetMode::Mixed, ResetMode::Hard][choice(f, 0).min(2)];
        vec![Op::Reset { target: target.clone(), mode }]
    })
    .danger())
}

pub fn create_tag(target: String, label: &str) -> Dialog {
    let fields = vec![
        required("Tag name", "", "v1.0.0"),
        Field::Text { label: "Message", value: String::new(), hint: "Optional: makes an annotated tag", required: false },
    ];
    Dialog::new(ic::TAG, "Create tag", format!("Tag {label}."), fields, "Create", move |f| {
        vec![Op::CreateTag { name: text(f, 0), target: target.clone(), message: text(f, 1) }]
    })
}

pub fn delete_tag(name: String) -> Dialog {
    let op = Op::DeleteTag { name: name.clone() };
    confirm(ic::DELETE, "Delete tag", format!("Delete local tag {name}."), "Delete", vec![op]).danger()
}

pub fn push_tag(snap: &Snapshot, tag: String, delete: bool) -> Result<Dialog, String> {
    let remote = snap.remotes.first().ok_or("this repository has no remotes; add one first")?.name.clone();
    let (title, verb) = if delete { ("Delete remote tag", "Delete") } else { ("Push tag", "Push") };
    let body = if delete { format!("Delete tag {tag} on {remote}.") } else { format!("Push tag {tag} to {remote}.") };
    let dialog = confirm(ic::TAG, title, body, verb, vec![Op::PushTag { remote, tag, delete }]);
    Ok(if delete { dialog.danger() } else { dialog })
}

pub fn stash() -> Dialog {
    let fields = vec![
        Field::Text { label: "Message", value: String::new(), hint: "Optional", required: false },
        toggle("Include untracked files", true),
    ];
    Dialog::new(ic::STASH, "Stash changes", "Shelve local changes and clean the working tree.".into(), fields, "Stash", |f| {
        vec![Op::StashPush { message: text(f, 0), include_untracked: on(f, 1) }]
    })
}

pub fn drop_stash(name: String, message: &str) -> Dialog {
    let body = format!("Drop {name} \u{201c}{message}\u{201d}. Its changes are lost.");
    confirm(ic::DELETE, "Drop stash", body, "Drop", vec![Op::StashDrop { name }]).danger()
}

pub fn discard(changes: Vec<Change>) -> Dialog {
    let mut names: Vec<&str> = changes.iter().take(6).map(|c| c.path.as_str()).collect();
    if changes.len() > names.len() {
        names.push("\u{2026}");
    }
    let body = format!("Discard changes in {} file(s)? This cannot be undone.\n\n{}", changes.len(), names.join("\n"));
    confirm(ic::UNDO, "Discard changes", body, "Discard", vec![Op::Discard { changes }]).danger()
}

pub fn add_remote(first: bool) -> Dialog {
    let fields = vec![required("Name", if first { "origin" } else { "" }, "origin"), required("URL", "", "https://example.com/team/repo.git")];
    Dialog::new(ic::CLOUD, "Add remote", String::new(), fields, "Add", |f| vec![Op::AddRemote { name: text(f, 0), url: text(f, 1) }])
}

pub fn remove_remote(name: String) -> Dialog {
    let body = format!("Remove remote {name} and its remote-tracking branches.");
    confirm(ic::DELETE, "Remove remote", body, "Remove", vec![Op::RemoveRemote { name }]).danger()
}

pub fn cherry_pick(snap: &Snapshot, commit: &Commit) -> Result<Dialog, String> {
    let branch = current_branch(snap)?;
    let body = format!("Apply {} \u{201c}{}\u{201d} on top of {branch}.", short(&commit.sha), commit.subject);
    let op = Op::CherryPick { sha: commit.sha.clone(), mainline: commit.parents.len() > 1 };
    Ok(confirm(ic::PICK, "Cherry-pick", body, "Cherry-pick", vec![op]))
}

pub fn revert(commit: &Commit) -> Dialog {
    let body = format!("Create a commit that undoes {} \u{201c}{}\u{201d}.", short(&commit.sha), commit.subject);
    let op = Op::Revert { sha: commit.sha.clone(), mainline: commit.parents.len() > 1 };
    confirm(ic::UNDO, "Revert commit", body, "Revert", vec![op])
}

pub fn checkout_detached(target: String, label: &str) -> Dialog {
    let body = format!("Check out {label}. HEAD will be detached: commits made there belong to no branch.");
    confirm(ic::COMMIT, "Check out commit", body, "Check out", vec![Op::Checkout { target }])
}
