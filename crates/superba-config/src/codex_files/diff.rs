//! Line diffs for the save and restore previews (Myers' algorithm after
//! trimming the common head and tail; very different files fall back to
//! "everything removed, everything added").

/// What happened to a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffTag {
    /// In both.
    Equal,
    /// Only in the old text.
    Delete,
    /// Only in the new text.
    Insert,
}

/// One line of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    /// What happened.
    pub tag: DiffTag,
    /// 1-based line in the old text.
    pub old: Option<usize>,
    /// 1-based line in the new text.
    pub new: Option<usize>,
    /// The line (without its line break).
    pub text: String,
}

/// A run of changes with context around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// First old line (1-based; 0 for an empty old side).
    pub old_start: usize,
    /// Old lines covered.
    pub old_len: usize,
    /// First new line (1-based; 0 for an empty new side).
    pub new_start: usize,
    /// New lines covered.
    pub new_len: usize,
    /// The lines.
    pub lines: Vec<DiffLine>,
}

/// Lines added and removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiffStat {
    /// Added.
    pub added: usize,
    /// Removed.
    pub removed: usize,
}

/// Edits explored at most before giving up on a minimal diff.
const MAX_EDITS: usize = 4000;
/// Entries of the backtracking trace kept at most (memory bound).
const MAX_TRACE: usize = 16_000_000;

fn split(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let t = text.strip_suffix('\n').unwrap_or(text);
    t.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// The edit script between `a` and `b` as (tag, index in a, index in b).
fn myers(a: &[&str], b: &[&str]) -> Option<Vec<(DiffTag, usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    if max == 0 {
        return Some(Vec::new());
    }
    let off = max as isize;
    let mut v = vec![0isize; 2 * max + 2];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let limit = max.min(MAX_EDITS).min(MAX_TRACE / (2 * max + 2));
    let mut found = None;
    'outer: for d in 0..=limit as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + off) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let d_end = found?;
    // Backtrack.
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=d_end).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let idx = (k + off) as usize;
        let prev_k = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + off) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push((DiffTag::Equal, x as usize, y as usize));
        }
        if x == prev_x {
            y -= 1;
            ops.push((DiffTag::Insert, x as usize, y as usize));
        } else {
            x -= 1;
            ops.push((DiffTag::Delete, x as usize, y as usize));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push((DiffTag::Equal, x as usize, y as usize));
    }
    ops.reverse();
    Some(ops)
}

/// Every line of `old` and `new` with what happened to it.
pub fn diff_lines(old: &str, new: &str) -> Vec<DiffLine> {
    let a = split(old);
    let b = split(new);
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    let mut out = Vec::with_capacity(a.len().max(b.len()));
    let line = |tag, old: Option<usize>, new: Option<usize>, text: &str| DiffLine {
        tag,
        old: old.map(|i| i + 1),
        new: new.map(|i| i + 1),
        text: text.to_owned(),
    };
    for (i, text) in a.iter().enumerate().take(head) {
        out.push(line(DiffTag::Equal, Some(i), Some(i), text));
    }
    match myers(ma, mb) {
        Some(ops) => {
            for (tag, x, y) in ops {
                let (ox, oy) = (head + x, head + y);
                out.push(match tag {
                    DiffTag::Equal => line(tag, Some(ox), Some(oy), a[ox]),
                    DiffTag::Delete => line(tag, Some(ox), None, a[ox]),
                    DiffTag::Insert => line(tag, None, Some(oy), b[oy]),
                });
            }
        }
        None => {
            for (i, t) in ma.iter().enumerate() {
                out.push(line(DiffTag::Delete, Some(head + i), None, t));
            }
            for (i, t) in mb.iter().enumerate() {
                out.push(line(DiffTag::Insert, None, Some(head + i), t));
            }
        }
    }
    for i in 0..tail {
        let (ox, oy) = (a.len() - tail + i, b.len() - tail + i);
        out.push(line(DiffTag::Equal, Some(ox), Some(oy), a[ox]));
    }
    out
}

/// Lines added and removed.
pub fn stat(lines: &[DiffLine]) -> DiffStat {
    let mut s = DiffStat::default();
    for l in lines {
        match l.tag {
            DiffTag::Insert => s.added += 1,
            DiffTag::Delete => s.removed += 1,
            DiffTag::Equal => {}
        }
    }
    s
}

/// The changes of `lines` grouped into hunks with `context` unchanged lines
/// around them (none when nothing changed).
pub fn hunks(lines: &[DiffLine], context: usize) -> Vec<Hunk> {
    let changed: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.tag != DiffTag::Equal)
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < changed.len() {
        let start = changed[i].saturating_sub(context);
        let mut end = (changed[i] + context + 1).min(lines.len());
        let mut j = i + 1;
        while j < changed.len() && changed[j] <= end + context {
            end = (changed[j] + context + 1).min(lines.len());
            j += 1;
        }
        let slice = &lines[start..end];
        let old_start = slice.iter().find_map(|l| l.old).unwrap_or(0);
        let new_start = slice.iter().find_map(|l| l.new).unwrap_or(0);
        out.push(Hunk {
            old_start,
            old_len: slice.iter().filter(|l| l.old.is_some()).count(),
            new_start,
            new_len: slice.iter().filter(|l| l.new.is_some()).count(),
            lines: slice.to_vec(),
        });
        i = j;
    }
    out
}

/// A unified diff (`--- old` / `+++ new` / `@@ ... @@`), empty when the texts
/// have the same lines.
pub fn unified(old_name: &str, new_name: &str, old: &str, new: &str, context: usize) -> String {
    let lines = diff_lines(old, new);
    let hs = hunks(&lines, context);
    if hs.is_empty() {
        return String::new();
    }
    let mut out = format!("--- {old_name}\n+++ {new_name}\n");
    for h in hs {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h.old_start, h.old_len, h.new_start, h.new_len
        ));
        for l in h.lines {
            out.push(match l.tag {
                DiffTag::Equal => ' ',
                DiffTag::Delete => '-',
                DiffTag::Insert => '+',
            });
            out.push_str(&l.text);
            out.push('\n');
        }
    }
    out
}
