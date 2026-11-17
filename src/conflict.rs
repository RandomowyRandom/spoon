//! Merge conflict model: parse a file with conflict markers, resolve each conflict by
//! picking lines from either side and/or editing the result, then render the file.
//! Pure logic plus two small file helpers; the editor UI lives in `merge_editor.rs`.

use std::path::Path;

use crate::git::Change;

/// Files larger than this are left to an external editor.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// "Current" is what HEAD has (git's "ours"); "Incoming" is what is being merged or
/// replayed (git's "theirs").
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Current,
    Incoming,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Take {
    Current,
    Incoming,
    /// Both sides, current lines first.
    Both,
    /// Remove the conflicting lines of both sides.
    Neither,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub current: Vec<String>,
    pub incoming: Vec<String>,
    pub current_label: String,
    pub incoming_label: String,
    /// The marker block exactly as found; written back while unresolved.
    raw: Vec<String>,
    picked_current: Vec<bool>,
    picked_incoming: Vec<bool>,
    /// The side whose picked lines come first in the generated result.
    first: Side,
    /// The resolution: lines, each ended by '\n'. Empty means no lines.
    pub result: String,
    /// The result was typed into: from then on picks add and remove single lines
    /// instead of regenerating the text.
    manual: bool,
    pub resolved: bool,
}

/// Lines of a result text. A final '\n' ends the last line; it does not start another.
fn text_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.strip_suffix('\n').unwrap_or(text).split('\n').collect()
}

fn join_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> String {
    lines.into_iter().flat_map(|line| [line, "\n"]).collect()
}

impl Conflict {
    fn new(current: Vec<String>, incoming: Vec<String>, current_label: &str, incoming_label: &str, raw: Vec<String>) -> Self {
        Self {
            picked_current: vec![false; current.len()],
            picked_incoming: vec![false; incoming.len()],
            current,
            incoming,
            current_label: current_label.into(),
            incoming_label: incoming_label.into(),
            raw,
            first: Side::Current,
            result: String::new(),
            manual: false,
            resolved: false,
        }
    }

    pub fn lines(&self, side: Side) -> &[String] {
        match side {
            Side::Current => &self.current,
            Side::Incoming => &self.incoming,
        }
    }

    fn picks(&mut self, side: Side) -> &mut Vec<bool> {
        match side {
            Side::Current => &mut self.picked_current,
            Side::Incoming => &mut self.picked_incoming,
        }
    }

    pub fn is_picked(&self, side: Side, index: usize) -> bool {
        let picks = match side {
            Side::Current => &self.picked_current,
            Side::Incoming => &self.picked_incoming,
        };
        picks.get(index).copied().unwrap_or(false)
    }

    fn any_picked(&self) -> bool {
        self.picked_current.iter().chain(&self.picked_incoming).any(|p| *p)
    }

    pub fn result_lines(&self) -> Vec<&str> {
        text_lines(&self.result)
    }

    /// Picked lines: the side picked first leads, each side in its own order.
    fn generated(&self) -> String {
        let picked = |side: Side| self.lines(side).iter().enumerate().filter(move |(i, _)| self.is_picked(side, *i)).map(|(_, line)| line.as_str());
        match self.first {
            Side::Current => join_lines(picked(Side::Current).chain(picked(Side::Incoming))),
            Side::Incoming => join_lines(picked(Side::Incoming).chain(picked(Side::Current))),
        }
    }

    /// Picks or unpicks one line.
    pub fn toggle(&mut self, side: Side, index: usize) {
        let Some(line) = self.lines(side).get(index).cloned() else { return };
        let was = self.is_picked(side, index);
        if !self.any_picked() {
            self.first = side;
        }
        self.picks(side)[index] = !was;
        if !self.manual {
            self.result = self.generated();
            self.resolved = self.any_picked();
            return;
        }
        // Hand-edited text: add the line at the end, or take its last occurrence out.
        let mut lines = text_lines(&self.result);
        match (was, lines.iter().rposition(|l| *l == line)) {
            (false, _) => lines.push(&line),
            (true, Some(position)) => drop(lines.remove(position)),
            (true, None) => {}
        }
        self.result = join_lines(lines);
    }

    /// Picks every line from `from` through `to` on one side (a Shift-click range).
    pub fn pick_range(&mut self, side: Side, from: usize, to: usize) {
        for index in from.min(to)..=from.max(to) {
            if !self.is_picked(side, index) {
                self.toggle(side, index);
            }
        }
    }

    /// How many lines of `side` are picked.
    pub fn picked_count(&self, side: Side) -> usize {
        (0..self.lines(side).len()).filter(|index| self.is_picked(side, *index)).count()
    }

    /// Picks or unpicks every line of one side (the chunk checkbox).
    pub fn set_side(&mut self, side: Side, picked: bool) {
        for index in 0..self.lines(side).len() {
            if self.is_picked(side, index) != picked {
                self.toggle(side, index);
            }
        }
    }

    /// Resolved by dropping the lines of both sides.
    pub fn dropped(&self) -> bool {
        self.resolved && self.result.is_empty() && !self.any_picked()
    }

    /// Resolves the whole conflict in one step, replacing any picks and edits.
    pub fn take(&mut self, take: Take) {
        let (current, incoming) = match take {
            Take::Current => (true, false),
            Take::Incoming => (false, true),
            Take::Both => (true, true),
            Take::Neither => (false, false),
        };
        self.picked_current.fill(current);
        self.picked_incoming.fill(incoming);
        self.first = Side::Current;
        self.manual = false;
        self.result = self.generated();
        self.resolved = true;
    }

    /// Back to unresolved, dropping picks and edits.
    pub fn reset(&mut self) {
        self.picked_current.fill(false);
        self.picked_incoming.fill(false);
        self.result.clear();
        self.manual = false;
        self.resolved = false;
    }

    /// The user typed in the result. Text equal to the current result is not an edit.
    pub fn edit(&mut self, text: &str) {
        if text != self.result {
            self.result = text.to_string();
            self.manual = true;
            self.resolved = true;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    /// Lines outside any conflict.
    Plain(Vec<String>),
    Conflict(Conflict),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub segments: Vec<Segment>,
    crlf: bool,
    trailing_newline: bool,
}

/// `<<<<<<<`, `|||||||` or `>>>>>>>`, alone or followed by a label.
fn marker(line: &str, symbol: char) -> Option<&str> {
    let rest = line.strip_prefix(&symbol.to_string().repeat(7))?;
    (rest.is_empty() || rest.starts_with(' ')).then(|| rest.trim())
}

/// Reads one complete conflict block starting at `start`. Returns it with the index
/// of the line after it, or `None` when the block is malformed or cut off.
fn read_conflict(lines: &[&str], start: usize) -> Option<(Conflict, usize)> {
    let current_label = marker(lines[start], '<')?;
    let (mut current, mut incoming) = (Vec::new(), Vec::new());
    let mut section = 0; // 0 = current, 1 = base (diff3 style, not offered), 2 = incoming
    for (offset, line) in lines[start + 1..].iter().enumerate() {
        let end = start + 1 + offset;
        if marker(line, '<').is_some() {
            return None;
        }
        if section == 0 && marker(line, '|').is_some() {
            section = 1;
        } else if section < 2 && *line == "=======" {
            section = 2;
        } else if let (2, Some(incoming_label)) = (section, marker(line, '>')) {
            let raw = lines[start..=end].iter().map(|l| l.to_string()).collect();
            return Some((Conflict::new(current, incoming, current_label, incoming_label, raw), end + 1));
        } else if section == 0 {
            current.push(line.to_string());
        } else if section == 2 {
            incoming.push(line.to_string());
        }
    }
    None
}

impl Document {
    pub fn parse(text: &str) -> Self {
        let lines: Vec<&str> = text_lines(text).into_iter().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        let mut segments = Vec::new();
        let mut plain: Vec<String> = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let Some((conflict, next)) = read_conflict(&lines, index) else {
                plain.push(lines[index].to_string());
                index += 1;
                continue;
            };
            if !plain.is_empty() {
                segments.push(Segment::Plain(std::mem::take(&mut plain)));
            }
            segments.push(Segment::Conflict(conflict));
            index = next;
        }
        if !plain.is_empty() {
            segments.push(Segment::Plain(plain));
        }
        Self { segments, crlf: text.contains("\r\n"), trailing_newline: text.ends_with('\n') }
    }

    pub fn conflicts(&self) -> impl Iterator<Item = &Conflict> {
        self.segments.iter().filter_map(|s| match s {
            Segment::Conflict(conflict) => Some(conflict),
            Segment::Plain(_) => None,
        })
    }

    pub fn conflicts_mut(&mut self) -> impl Iterator<Item = &mut Conflict> {
        self.segments.iter_mut().filter_map(|s| match s {
            Segment::Conflict(conflict) => Some(conflict),
            Segment::Plain(_) => None,
        })
    }

    pub fn total(&self) -> usize {
        self.conflicts().count()
    }

    pub fn resolved(&self) -> usize {
        self.conflicts().filter(|c| c.resolved).count()
    }

    /// The file text: resolved conflicts as their result, unresolved ones with their
    /// markers untouched. Line endings and the final newline follow the original.
    pub fn render(&self) -> String {
        let mut lines: Vec<&str> = Vec::new();
        for segment in &self.segments {
            match segment {
                Segment::Plain(plain) => lines.extend(plain.iter().map(String::as_str)),
                Segment::Conflict(c) if c.resolved => lines.extend(c.result_lines()),
                Segment::Conflict(c) => lines.extend(c.raw.iter().map(String::as_str)),
            }
        }
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let mut text = lines.join(eol);
        if self.trailing_newline && !lines.is_empty() {
            text.push_str(eol);
        }
        text
    }
}

/// What can be done with a conflicted file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FileState {
    /// Text with conflict markers: the editor can merge it.
    Text,
    /// Binary, or too large to edit: only one whole side can be taken.
    Binary,
    /// Both sides changed it, but the markers are already gone from the file
    /// (edited elsewhere): it only needs staging.
    Resolved,
    /// One side deleted, renamed or added the file: there is nothing to merge.
    OneSided,
}

pub fn file_state(repo: &Path, change: &Change) -> FileState {
    if !matches!((change.index, change.work), ('U', 'U') | ('A', 'A')) {
        return FileState::OneSided;
    }
    let file = repo.join(&change.path);
    // A directory here is a submodule in conflict.
    let Ok(meta) = std::fs::metadata(&file).map_err(drop).and_then(|m| if m.is_dir() { Err(()) } else { Ok(m) }) else {
        return FileState::OneSided;
    };
    if meta.len() > MAX_BYTES {
        return FileState::Binary;
    }
    match std::fs::read(&file).map(String::from_utf8) {
        Ok(Ok(text)) if text.contains('\0') => FileState::Binary,
        Ok(Ok(text)) if Document::parse(&text).total() == 0 => FileState::Resolved,
        Ok(Ok(_)) => FileState::Text,
        Ok(Err(_)) => FileState::Binary,
        Err(_) => FileState::OneSided,
    }
}

/// Reads a conflicted file from the work tree. Returns its text and the parsed document.
pub fn load(repo: &Path, path: &str) -> Result<(String, Document), String> {
    let file = repo.join(path);
    let size = std::fs::metadata(&file).map_err(|_| format!("{path} is not in the work tree: one side deleted it. Resolve it with \"use ours\" or \"use theirs\"."))?.len();
    if size > MAX_BYTES {
        return Err(format!("{path} is too large for the conflict editor ({} MB)", size / (1024 * 1024)));
    }
    let bytes = std::fs::read(&file).map_err(|e| format!("cannot read {path}: {e}"))?;
    if bytes.contains(&0) {
        return Err(format!("{path} is a binary file. Resolve it with \"use ours\" or \"use theirs\"."));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8 text"))?;
    let document = Document::parse(&text);
    if document.total() == 0 {
        return Err(format!("{path} has no conflict markers. Resolve it with \"use ours\" or \"use theirs\"."));
    }
    Ok((text, document))
}

/// Writes the resolved text, unless the file changed on disk since `original` was
/// read: then someone else edited it and overwriting would lose that work.
pub fn save(repo: &Path, path: &str, original: &str, content: &str) -> Result<(), String> {
    let file = repo.join(path);
    match std::fs::read(&file) {
        Ok(on_disk) if on_disk == original.as_bytes() => {}
        Ok(_) => return Err(format!("{path} changed on disk while it was open here. Reopen it to resolve the current version.")),
        Err(e) => return Err(format!("cannot read {path}: {e}")),
    }
    std::fs::write(&file, content).map_err(|e| format!("cannot write {path}: {e}"))
}
