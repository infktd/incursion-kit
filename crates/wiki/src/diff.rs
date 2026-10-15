//! Line diffs between two revisions.

use similar::{ChangeTag, TextDiff};

use crate::model::DiffLine;

/// The diff of two bodies: every line (with its op and line numbers), the counts and a unified
/// diff with three lines of context.
pub fn diff_lines(old: &str, new: &str) -> (Vec<DiffLine>, usize, usize, String) {
    let diff = TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    let (mut insertions, mut deletions) = (0, 0);
    for change in diff.iter_all_changes() {
        let op = match change.tag() {
            ChangeTag::Equal => "equal",
            ChangeTag::Insert => {
                insertions += 1;
                "insert"
            }
            ChangeTag::Delete => {
                deletions += 1;
                "delete"
            }
        };
        lines.push(DiffLine {
            op: op.to_string(),
            old_line: change.old_index().map(|i| i + 1),
            new_line: change.new_index().map(|i| i + 1),
            text: change.value().trim_end_matches(['\r', '\n']).to_string(),
        });
    }
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header("old", "new")
        .to_string();
    (lines, insertions, deletions, unified)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diffs_lines() {
        let (lines, ins, del, unified) = diff_lines("a\nb\nc\n", "a\nB\nc\nd\n");
        assert_eq!((ins, del), (2, 1));
        let ops: Vec<_> = lines.iter().map(|l| l.op.as_str()).collect();
        assert_eq!(ops, vec!["equal", "delete", "insert", "equal", "insert"]);
        assert_eq!(lines[1].old_line, Some(2));
        assert_eq!(lines[2].new_line, Some(2));
        assert!(unified.contains("-b\n+B\n"), "{unified}");
    }
}
