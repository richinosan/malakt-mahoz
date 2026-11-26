//! 文字列の差分。変更履歴で、変わったところを赤（消えた）と緑（増えた）で見せるのに使う。
//!
//! 短い値は文字ごとに比べ、説明のような長い文は行ごとに比べて、変わったところの前後だけ（ハンク）を出す。
//! 最長共通部分列は O(n×m) なので、先頭と末尾の同じところを除いてから比べ、それでも大きすぎるときは
//! 全部を入れ替えたものとして扱う。

/// 1 つの部分が、両方にあるか、消えたか、増えたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Equal,
    Delete,
    Insert,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub kind: PartKind,
    pub text: String,
}

impl Part {
    pub fn new(kind: PartKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}

/// 行ごとの差分の 1 行。`before` は変更前の側に、`after` は変更後の側に出す部分。
/// 変わらない行は両方に出る。片方にない行は空。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub before: Vec<Part>,
    pub after: Vec<Part>,
}

/// 変わったところと、その前後の変わらない行のまとまり。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hunk {
    pub lines: Vec<Line>,
}

/// 比べる表の大きさの上限（マスの数）。超えたら全部を入れ替えたものとして扱う。
const MAX_CELLS: usize = 2_000_000;

/// 変わったところの前後に残す、変わらない行の数。
pub const HUNK_CONTEXT_LINES: usize = 2;

fn diff_sequences<T: PartialEq + Clone>(before: &[T], after: &[T]) -> Vec<(PartKind, T)> {
    let mut start = 0;
    while start < before.len() && start < after.len() && before[start] == after[start] {
        start += 1;
    }
    let (mut before_end, mut after_end) = (before.len(), after.len());
    while before_end > start && after_end > start && before[before_end - 1] == after[after_end - 1]
    {
        before_end -= 1;
        after_end -= 1;
    }
    let equal = |items: &[T]| -> Vec<(PartKind, T)> {
        items
            .iter()
            .map(|item| (PartKind::Equal, item.clone()))
            .collect()
    };
    let mut ops = equal(&before[..start]);
    let left = &before[start..before_end];
    let right = &after[start..after_end];
    if left.is_empty()
        || right.is_empty()
        || (left.len() + 1).saturating_mul(right.len() + 1) > MAX_CELLS
    {
        ops.extend(left.iter().map(|item| (PartKind::Delete, item.clone())));
        ops.extend(right.iter().map(|item| (PartKind::Insert, item.clone())));
    } else {
        let columns = right.len() + 1;
        let mut lengths = vec![0u32; (left.len() + 1) * columns];
        for row in (0..left.len()).rev() {
            for column in (0..right.len()).rev() {
                lengths[row * columns + column] = if left[row] == right[column] {
                    lengths[(row + 1) * columns + column + 1] + 1
                } else {
                    lengths[(row + 1) * columns + column].max(lengths[row * columns + column + 1])
                };
            }
        }
        let (mut row, mut column) = (0, 0);
        while row < left.len() && column < right.len() {
            if left[row] == right[column] {
                ops.push((PartKind::Equal, left[row].clone()));
                row += 1;
                column += 1;
            } else if lengths[(row + 1) * columns + column] >= lengths[row * columns + column + 1] {
                ops.push((PartKind::Delete, left[row].clone()));
                row += 1;
            } else {
                ops.push((PartKind::Insert, right[column].clone()));
                column += 1;
            }
        }
        ops.extend(
            left[row..]
                .iter()
                .map(|item| (PartKind::Delete, item.clone())),
        );
        ops.extend(
            right[column..]
                .iter()
                .map(|item| (PartKind::Insert, item.clone())),
        );
    }
    ops.extend(equal(&before[before_end..]));
    ops
}

/// 同じ種類が続くところをまとめる。`skip` の種類は、その側には出さない。
fn collapse(ops: &[(PartKind, char)], skip: PartKind) -> Vec<Part> {
    let mut parts: Vec<Part> = Vec::new();
    for (kind, c) in ops {
        if *kind == skip {
            continue;
        }
        match parts.last_mut() {
            Some(last) if last.kind == *kind => last.text.push(*c),
            _ => parts.push(Part::new(*kind, c.to_string())),
        }
    }
    parts
}

/// 文字ごとの差分。変更前の側と変更後の側に出す部分。
pub fn inline(before: &str, after: &str) -> (Vec<Part>, Vec<Part>) {
    if before == after {
        return (
            vec![Part::new(PartKind::Equal, before)],
            vec![Part::new(PartKind::Equal, after)],
        );
    }
    let left: Vec<char> = before.chars().collect();
    let right: Vec<char> = after.chars().collect();
    let ops = diff_sequences(&left, &right);
    (
        collapse(&ops, PartKind::Insert),
        collapse(&ops, PartKind::Delete),
    )
}

/// 続けて消えた行と増えた行は、同じ順で 1 行ずつ組にして文字ごとに比べる。
fn build_lines(ops: &[(PartKind, String)]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut index = 0;
    while index < ops.len() {
        let (kind, text) = &ops[index];
        if *kind == PartKind::Equal {
            lines.push(Line {
                before: vec![Part::new(PartKind::Equal, text.clone())],
                after: vec![Part::new(PartKind::Equal, text.clone())],
            });
            index += 1;
            continue;
        }
        let (mut deleted, mut inserted) = (Vec::new(), Vec::new());
        while index < ops.len() && ops[index].0 != PartKind::Equal {
            if ops[index].0 == PartKind::Delete {
                deleted.push(ops[index].1.as_str());
            } else {
                inserted.push(ops[index].1.as_str());
            }
            index += 1;
        }
        let paired = deleted.len().min(inserted.len());
        for pair in 0..paired {
            let (before, after) = inline(deleted[pair], inserted[pair]);
            lines.push(Line { before, after });
        }
        for text in &deleted[paired..] {
            lines.push(Line {
                before: vec![Part::new(PartKind::Delete, *text)],
                after: Vec::new(),
            });
        }
        for text in &inserted[paired..] {
            lines.push(Line {
                before: Vec::new(),
                after: vec![Part::new(PartKind::Insert, *text)],
            });
        }
    }
    lines
}

/// 行ごとの差分を、変わったところの前後 [`HUNK_CONTEXT_LINES`] 行ずつのまとまりにする。同じなら空。
pub fn hunks(before: &str, after: &str) -> Vec<Hunk> {
    if before == after {
        return Vec::new();
    }
    let split = |s: &str| -> Vec<String> {
        s.replace("\r\n", "\n")
            .split('\n')
            .map(str::to_owned)
            .collect()
    };
    let ops = diff_sequences(&split(before), &split(after));
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != PartKind::Equal)
        .map(|(i, _)| i)
        .collect();
    let Some(&first) = changed.first() else {
        return Vec::new();
    };
    let context = HUNK_CONTEXT_LINES;
    let mut hunks = Vec::new();
    let (mut start, mut end) = (first, first);
    let mut flush = |start: usize, end: usize| {
        let from = start.saturating_sub(context);
        let to = (end + context).min(ops.len() - 1);
        hunks.push(Hunk {
            lines: build_lines(&ops[from..=to]),
        });
    };
    for &index in &changed[1..] {
        if index - end <= context * 2 + 1 {
            end = index;
            continue;
        }
        flush(start, end);
        start = index;
        end = index;
    }
    flush(start, end);
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(parts: &[Part], kind: PartKind) -> String {
        parts
            .iter()
            .filter(|p| p.kind == kind)
            .map(|p| p.text.as_str())
            .collect()
    }

    #[test]
    fn inline_marks_only_what_changed() {
        let (before, after) = inline("会場の下見", "会場の予約");
        assert_eq!(text(&before, PartKind::Delete), "下見");
        assert_eq!(text(&after, PartKind::Insert), "予約");
        assert_eq!(text(&before, PartKind::Equal), "会場の");
        // 両側をつなげると元の文になる
        let join = |parts: &[Part]| parts.iter().map(|p| p.text.as_str()).collect::<String>();
        assert_eq!(join(&before), "会場の下見");
        assert_eq!(join(&after), "会場の予約");
        // 同じなら全部が変わらない部分
        let (same, _) = inline("a", "a");
        assert_eq!(same, vec![Part::new(PartKind::Equal, "a")]);
    }

    #[test]
    fn hunks_keep_only_lines_near_the_change() {
        let before = (1..=20).map(|n| format!("行{n}")).collect::<Vec<_>>();
        let mut after = before.clone();
        after[2] = "行3を直した".into();
        after[16] = "行17を直した".into();
        let found = hunks(&before.join("\n"), &after.join("\n"));
        assert_eq!(found.len(), 2, "離れた 2 か所は別のまとまり");
        // 前後 2 行と、変わった 1 行
        assert_eq!(found[0].lines.len(), 5);
        let changed = &found[0].lines[2];
        assert_eq!(text(&changed.after, PartKind::Insert), "を直した");
        assert!(hunks("同じ", "同じ").is_empty());
    }

    #[test]
    fn added_and_removed_lines_stand_alone() {
        let found = hunks("a\nb", "a\nb\nc");
        let last = found[0].lines.last().unwrap();
        assert!(last.before.is_empty());
        assert_eq!(last.after, vec![Part::new(PartKind::Insert, "c")]);
        let hunks = super::hunks("a\r\nb", "b");
        assert_eq!(
            hunks[0].lines[0].before,
            vec![Part::new(PartKind::Delete, "a")]
        );
    }

    #[test]
    fn huge_inputs_are_replaced_whole() {
        let a: String = "あ".repeat(3000);
        let b: String = "い".repeat(3000);
        let (before, after) = inline(&a, &b);
        assert_eq!(before, vec![Part::new(PartKind::Delete, a)]);
        assert_eq!(after, vec![Part::new(PartKind::Insert, b)]);
    }
}
