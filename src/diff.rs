//! Unified diff parser for the diff viewer.

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
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Diff {
    pub lines: Vec<Line>,
    pub binary: bool,
    pub added: usize,
    pub deleted: usize,
    /// Longest line in characters, for horizontal scrolling.
    pub max_chars: usize,
}

/// Start lines of a hunk header: `@@ -12,7 +14,9 @@ context`.
fn hunk_starts(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.split_whitespace().skip(1);
    let start = |s: &str| s.get(1..)?.split(',').next()?.parse::<u32>().ok();
    Some((start(parts.next()?)?, start(parts.next()?)?))
}

pub fn parse(raw: &str) -> Diff {
    let mut diff = Diff::default();
    let (mut old, mut new) = (0u32, 0u32);
    let mut in_hunk = false;

    for line in raw.lines() {
        if line.starts_with("@@") {
            if let Some((o, n)) = hunk_starts(line) {
                (old, new, in_hunk) = (o, n, true);
                diff.lines.push(Line { kind: LineKind::Hunk, old: None, new: None, text: line.into() });
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
        diff.lines.push(Line { kind, old: o, new: n, text });
    }
    diff.max_chars = diff.lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0);
    diff
}
