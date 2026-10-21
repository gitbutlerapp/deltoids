//! Which lines of one version of a file are the same lines in another:
//! how a line moved between a commit and the file today.

use crate::engine::{DiffOp, Snapshot};

/// The unchanged lines between an old and a new version of a text, as
/// runs of equal lines.
#[derive(Debug, Clone)]
pub struct LineMap {
    /// `(old_start, new_start, len)`, 0-based, in order.
    runs: Vec<(usize, usize, usize)>,
}

impl LineMap {
    /// Line up `old` with `new` through the line diff the rest of
    /// deltoids shows.
    pub fn new(old: &str, new: &str) -> Self {
        let runs = Snapshot::compute(old, new)
            .ops()
            .iter()
            .filter_map(|op| match *op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => Some((old_index, new_index, len)),
                _ => None,
            })
            .collect();
        LineMap { runs }
    }

    /// Where 1-based old `line` is in the new version, or `None` when
    /// the line changed.
    pub fn forward(&self, line: usize) -> Option<usize> {
        let index = line.checked_sub(1)?;
        self.runs.iter().find_map(|&(old, new, len)| {
            (old..old + len)
                .contains(&index)
                .then(|| new + index - old + 1)
        })
    }

    /// Where 1-based new `line` was in the old version, or `None` when
    /// the line changed.
    pub fn backward(&self, line: usize) -> Option<usize> {
        let index = line.checked_sub(1)?;
        self.runs.iter().find_map(|&(old, new, len)| {
            (new..new + len)
                .contains(&index)
                .then(|| old + index - new + 1)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_lines_follow_insertions_above_them() {
        let map = LineMap::new("a\nb\nc\n", "new\nnew\na\nb\nc\n");
        assert_eq!(map.forward(1), Some(3));
        assert_eq!(map.forward(3), Some(5));
        assert_eq!(map.backward(5), Some(3));
        assert_eq!(map.backward(1), None, "an inserted line has no old line");
    }

    #[test]
    fn a_changed_line_maps_nowhere_while_its_neighbours_still_do() {
        let map = LineMap::new("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!(map.forward(2), None);
        assert_eq!(map.backward(2), None);
        assert_eq!(map.forward(3), Some(3));
    }

    #[test]
    fn lines_past_either_end_map_nowhere() {
        let map = LineMap::new("a\n", "a\n");
        assert_eq!(map.forward(0), None);
        assert_eq!(map.forward(2), None);
        assert_eq!(map.backward(1), Some(1));
    }
}
