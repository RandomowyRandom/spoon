//! Unified diff parser for the diff viewer, and patches made from picked lines.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineKind {
    Hunk,
    Context,
    Added,
    Deleted,
    Note,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// For display: without the sign, tabs widened.
    pub text: String,
    /// The line as git wrote it, in `Diff::raw`: sign first, line end left out.
    span: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Diff {
    pub lines: Vec<Line>,
    pub binary: bool,
    pub added: usize,
    pub deleted: usize,
    /// Longest line in characters, for horizontal scrolling.
    pub max_chars: usize,
    /// Git's output, unchanged. Patches are cut from it so that no byte is lost.
    pub raw: String,
    /// Length of the file header: everything in `raw` before the first hunk.
    head: usize,
    files: usize,
}

/// Start lines of a hunk header: `@@ -12,7 +14,9 @@ context`.
fn hunk_starts(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.split_whitespace().skip(1);
    let start = |s: &str| s.get(1..)?.split(',').next()?.parse::<u32>().ok();
    Some((start(parts.next()?)?, start(parts.next()?)?))
}

pub fn parse(raw: &str) -> Diff {
    let mut diff = Diff { raw: raw.to_string(), head: raw.len(), ..Default::default() };
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;
    let mut at = 0;

    for piece in raw.split_inclusive('\n') {
        let start = at;
        at += piece.len();
        let exact = piece.strip_suffix('\n').unwrap_or(piece);
        let span = start..start + exact.len();
        let line = exact.strip_suffix('\r').unwrap_or(exact);
        diff.files += (line.starts_with("diff --git ") || line.starts_with("diff --cc ")) as usize;
        if line.starts_with("@@") {
            if let Some((o, n)) = hunk_starts(line) {
                (old, new, in_hunk) = (o, n, true);
                diff.head = diff.head.min(start);
                diff.lines.push(Line { kind: LineKind::Hunk, old: None, new: None, text: line.into(), span });
            }
            continue;
        }
        if !in_hunk {
            diff.binary |= line.starts_with("Binary files ") || line.starts_with("GIT binary patch");
            continue;
        }
        if line.starts_with("diff --git ") {
            in_hunk = false; // next file of a multi-file diff
            continue;
        }
        let text = line.get(1..).unwrap_or("").replace('\t', "    ");
        let (kind, o, n) = match line.as_bytes().first() {
            Some(b'+') => (LineKind::Added, None, Some(new)),
            Some(b'-') => (LineKind::Deleted, Some(old), None),
            Some(b'\\') => (LineKind::Note, None, None),
            _ => (LineKind::Context, Some(old), Some(new)),
        };
        old += o.is_some() as u32;
        new += n.is_some() as u32;
        diff.added += (kind == LineKind::Added) as usize;
        diff.deleted += (kind == LineKind::Deleted) as usize;
        diff.lines.push(Line { kind, old: o, new: n, text, span });
    }
    diff.max_chars = diff.lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0);
    diff
}

impl Diff {
    fn header(&self) -> impl Iterator<Item = &str> {
        self.raw[..self.head].lines()
    }

    /// Whether single lines of this diff can go into the index or out of it: the diff
    /// of one text file that is neither deleted nor renamed.
    pub fn patchable(&self) -> bool {
        let odd = ["deleted file mode ", "rename from ", "copy from "];
        self.files == 1 && !self.binary && !self.lines.is_empty() && self.raw.starts_with("diff --git ") && !self.header().any(|line| odd.iter().any(|start| line.starts_with(start)))
    }

    /// A patch of only the `picked` changes (indices into `lines`), for `git apply
    /// --cached`: as it is to stage them from a diff of unstaged changes, with
    /// `--reverse` to unstage them from a diff of staged ones (`reverse` says which).
    /// `None` when nothing picked is a change, or the diff is not `patchable`.
    ///
    /// A change left out must still match the file the patch is applied to. Staging
    /// applies to the old side, so a deletion left out stays as context and an
    /// addition left out goes. Unstaging applies to the new side: the other way round.
    pub fn patch(&self, picked: impl Fn(usize) -> bool, reverse: bool) -> Option<String> {
        if !self.patchable() {
            return None;
        }
        let is_picked = |i: usize| picked(i) && matches!(self.lines[i].kind, LineKind::Added | LineKind::Deleted);
        let mut hunks = String::new();
        // How much longer the new side is than the old, over the hunks written so far.
        let mut grown = 0i64;
        let mut i = 0;
        while i < self.lines.len() {
            let end = (i + 1..self.lines.len()).find(|&j| self.lines[j].kind == LineKind::Hunk).unwrap_or(self.lines.len());
            let (mut body, mut olds, mut news, mut kept) = (String::new(), 0i64, 0i64, false);
            for j in i + 1..end {
                let sign = match (self.lines[j].kind, is_picked(j)) {
                    (LineKind::Added, true) => Some('+'),
                    (LineKind::Deleted, true) => Some('-'),
                    (LineKind::Added, false) => reverse.then_some(' '),
                    (LineKind::Deleted, false) => (!reverse).then_some(' '),
                    // "No newline at end of file" belongs to the line before it.
                    (LineKind::Note, _) => kept.then_some('\\'),
                    _ => Some(' '),
                };
                if self.lines[j].kind != LineKind::Note {
                    kept = sign.is_some();
                }
                let Some(sign) = sign else { continue };
                olds += matches!(sign, ' ' | '-') as i64;
                news += matches!(sign, ' ' | '+') as i64;
                body.push(sign);
                body += self.raw[self.lines[j].span.clone()].get(1..).unwrap_or("");
                body.push('\n');
            }
            let starts = hunk_starts(&self.lines[i].text).filter(|_| (i + 1..end).any(&is_picked));
            i = end;
            let Some((old, new)) = starts else { continue };
            // The side the patch is applied to keeps its place; the other side moves by
            // what the earlier hunks add. A side without lines names the line before it.
            let first = |start: u32, count: i64| start as i64 + (count == 0) as i64;
            let (old, new) = if reverse { (first(new, news) - grown, new as i64) } else { (old as i64, first(old, olds) + grown) };
            let (old, new) = if reverse { (old - (olds == 0) as i64, new) } else { (old, new - (news == 0) as i64) };
            hunks += &format!("@@ -{old},{olds} +{new},{news} @@\n{body}");
            grown += news - olds;
        }
        if hunks.is_empty() {
            return None;
        }
        // Unstaging part of a new file leaves the rest of it in the index: a change to
        // the file, not its removal.
        let partly_new = reverse && (0..self.lines.len()).any(|i| self.lines[i].kind == LineKind::Added && !is_picked(i));
        let mut patch = String::new();
        for line in self.header() {
            let old_name = |new_name: &str| new_name.strip_prefix("\"b/").map(|rest| format!("\"a/{rest}")).or_else(|| new_name.strip_prefix("b/").map(|rest| format!("a/{rest}")));
            let line = match line {
                "--- /dev/null" if partly_new => self.header().find_map(|l| old_name(l.strip_prefix("+++ ")?)).map(|name| format!("--- {name}"))?,
                l if l.starts_with("new file mode ") && partly_new => continue,
                l if ["diff --git ", "--- ", "+++ ", "new file mode "].iter().any(|start| l.starts_with(start)) => l.to_string(),
                _ => continue,
            };
            patch += &line;
            patch.push('\n');
        }
        Some(patch + &hunks)
    }
}
