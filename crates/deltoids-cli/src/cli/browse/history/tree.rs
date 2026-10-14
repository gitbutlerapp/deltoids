//! The branches screen's rows: a flat branch list in plain git, and the
//! workspace drawn the way `but status` draws it in GitButler.
//!
//! Uncommitted files stay in the Files panel. The common base closes
//! the applied stacks, as `but status` draws it. Only branch and commit
//! rows can be selected; graph connectors and the base are there to
//! read.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use deltoids::render_tui::rgb_to_color;
use deltoids::{Theme, git};

use super::branch_row;
use crate::cli::browse::vcs::{Base, Branch, OverviewContent, Stack, StatusCommit};

/// What a selection holds on to across refreshes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Key {
    Branch(String),
    Commit(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A line of graph alone: `┊`, `┊│`, `├╯`, or blank.
    Connector(&'static str),
    /// A branch; `notch` is `╭` or `├` inside a stack, `None` outside.
    Branch { index: usize, notch: Option<char> },
    Commit {
        branch: usize,
        commit: usize,
        id_width: usize,
    },
    /// `┊ upstream +2`: commits the upstream branch has past the base.
    Upstream,
    /// `┴ 3917b63 (common base) 2026-10-14 init`.
    Base,
    /// The heading over branches outside the workspace.
    Unapplied,
}

pub(super) struct Tree {
    /// Whether to draw `but status`; otherwise a flat branch list.
    workspace: bool,
    /// Stacked branches top to bottom, then the others.
    branches: Vec<Branch>,
    base: Option<Base>,
    rows: Vec<Row>,
}

impl Tree {
    pub(super) fn new(content: OverviewContent) -> Self {
        let workspace = content.stacks.is_some();
        let mut tree = Self {
            workspace,
            branches: Vec::new(),
            base: content.base,
            rows: Vec::new(),
        };
        if workspace {
            for stack in content.stacks.into_iter().flatten() {
                tree.push_stack(stack);
            }
            if tree.base.as_ref().is_some_and(|base| base.behind > 0) {
                tree.rows.push(Row::Upstream);
            }
            if tree.base.is_some() {
                tree.rows.push(Row::Base);
            }
            if !content.others.is_empty() {
                tree.rows.push(Row::Connector(""));
                tree.rows.push(Row::Unapplied);
            }
        }
        for branch in content.others {
            tree.push_branch(branch, None);
        }
        tree
    }

    /// A stack's branches, each with its commits, closed by `├╯` and a
    /// spacer.
    fn push_stack(&mut self, stack: Stack) {
        for (position, branch) in stack.branches.into_iter().enumerate() {
            if position > 0 {
                self.rows.push(Row::Connector("┊│"));
            }
            let notch = if position == 0 { '╭' } else { '├' };
            self.push_branch(branch, Some(notch));
        }
        self.rows.push(Row::Connector("├╯"));
        self.rows.push(Row::Connector("┊"));
    }

    fn push_branch(&mut self, branch: Branch, notch: Option<char>) {
        let index = self.branches.len();
        self.rows.push(Row::Branch { index, notch });
        let id_width = branch.commits.iter().map(|c| c.id.len()).max().unwrap_or(0);
        self.rows
            .extend((0..branch.commits.len()).map(|commit| Row::Commit {
                branch: index,
                commit,
                id_width,
            }));
        self.branches.push(branch);
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn is_selectable(&self, row: usize) -> bool {
        matches!(
            self.rows.get(row),
            Some(Row::Branch { .. } | Row::Commit { .. })
        )
    }

    /// `row` if it can be selected, else the nearest row that can,
    /// looking `forward` first. `row` itself when none can.
    pub(super) fn snap(&self, row: usize, forward: bool) -> usize {
        let len = self.len();
        let row = row.min(len.saturating_sub(1));
        let ahead = (row..len).find(|&r| self.is_selectable(r));
        let behind = (0..=row).rev().find(|&r| self.is_selectable(r));
        let (first, second) = if forward {
            (ahead, behind)
        } else {
            (behind, ahead)
        };
        first.or(second).unwrap_or(row)
    }

    /// The branch on `row`.
    pub(super) fn branch(&self, row: usize) -> Option<&Branch> {
        match self.rows.get(row)? {
            Row::Branch { index, .. } => self.branches.get(*index),
            _ => None,
        }
    }

    /// The commit on `row`.
    pub(super) fn commit(&self, row: usize) -> Option<&git::Commit> {
        match self.rows.get(row)? {
            Row::Commit { branch, commit, .. } => {
                Some(&self.branches.get(*branch)?.commits.get(*commit)?.commit)
            }
            _ => None,
        }
    }

    pub(super) fn key(&self, row: usize) -> Option<Key> {
        if let Some(branch) = self.branch(row) {
            return Some(Key::Branch(branch.name.clone()));
        }
        self.commit(row).map(|c| Key::Commit(c.id.clone()))
    }

    pub(super) fn find(&self, key: &Key) -> Option<usize> {
        (0..self.len()).find(|&row| self.key(row).as_ref() == Some(key))
    }

    pub(super) fn lines(&self, now: i64, theme: &Theme) -> Vec<Line<'static>> {
        let style = Styles::new(theme);
        self.rows
            .iter()
            .map(|row| match *row {
                Row::Connector(text) => Line::raw(text),
                Row::Branch { index, notch } => {
                    let branch = &self.branches[index];
                    match notch {
                        Some(notch) => stacked_branch_line(branch, notch, &style),
                        None if self.workspace => Line::from(vec![
                            Span::raw("   ["),
                            Span::raw(branch.name.clone()),
                            Span::raw("]"),
                        ]),
                        None => branch_row(branch, now, theme),
                    }
                }
                Row::Commit {
                    branch,
                    commit,
                    id_width,
                } => commit_line(&self.branches[branch].commits[commit], id_width, &style),
                Row::Upstream => {
                    let behind = self.base.as_ref().map_or(0, |base| base.behind);
                    Line::from(vec![
                        Span::raw("┊ "),
                        Span::styled(format!("upstream +{behind}"), style.muted),
                    ])
                }
                Row::Base => self
                    .base
                    .as_ref()
                    .map_or_else(Line::default, |base| base_line(base, &style)),
                Row::Unapplied => Line::from(Span::styled("unapplied", style.muted)),
            })
            .collect()
    }
}

/// `but status`'s style roles, in the theme's colors.
struct Styles {
    cli_id: Style,
    branch: Style,
    commit_id: Style,
    muted: Style,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        let fg = |rgb| Style::default().fg(rgb_to_color(rgb));
        Self {
            cli_id: fg(theme.separator).add_modifier(Modifier::BOLD),
            branch: fg(theme.status_added),
            commit_id: fg(theme.status_typechange),
            muted: fg(theme.muted),
        }
    }
}

/// `┊╭┄ to [top-feature]`, with `(no commits)` after an empty branch.
fn stacked_branch_line(branch: &Branch, notch: char, style: &Styles) -> Line<'static> {
    let mut spans = vec![
        Span::raw(format!("┊{notch}┄ ")),
        Span::styled(branch.id.clone().unwrap_or_default(), style.cli_id),
        Span::raw(" ["),
        Span::styled(branch.name.clone(), style.branch),
        Span::raw("]"),
    ];
    if branch.commits.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled("(no commits)", style.muted));
    }
    Line::from(spans)
}

/// `┴ 3917b63 (common base) 2026-10-14 init`.
fn base_line(base: &Base, style: &Styles) -> Line<'static> {
    Line::from(vec![
        Span::raw("┴ "),
        Span::styled(base.id.chars().take(7).collect::<String>(), style.commit_id),
        Span::raw(" "),
        Span::styled("(common base)", style.muted),
        Span::raw(format!(" {} ", base.date)),
        Span::raw(base.summary.clone()),
    ])
}

/// `┊●   swuu top`, the ID padded to the branch's widest.
fn commit_line(commit: &StatusCommit, id_width: usize, style: &Styles) -> Line<'static> {
    let pad = id_width.saturating_sub(commit.id.len());
    let subject = if commit.commit.summary.is_empty() {
        Span::styled("(no commit message)", style.muted)
    } else {
        Span::raw(commit.commit.summary.clone())
    };
    Line::from(vec![
        Span::raw("┊●   "),
        Span::styled(commit.id.clone(), style.commit_id),
        Span::raw(" ".repeat(pad + 1)),
        subject,
    ])
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::cli::browse::vcs::test_support::branch;

    const NOW: i64 = 1_000_000;

    fn content() -> OverviewContent {
        let commit = git::Commit {
            id: "ba53cbb5d02a88c42ed0fba5bb94092e594eeaad".into(),
            short_id: "ba53cbb".into(),
            summary: "top".into(),
            message: "top".into(),
            author: "t".into(),
            email: "t@t".into(),
            time: NOW,
        };
        let mut top = branch("top-feature");
        top.id = Some("to".into());
        top.commits = vec![StatusCommit {
            id: "swuu".into(),
            commit,
        }];
        OverviewContent {
            stacks: Some(vec![Stack {
                branches: vec![top],
            }]),
            base: Some(Base {
                id: "3917b63b1afceb59f7c748f6bfe40c5c8bceeb93".into(),
                date: "2026-10-14".into(),
                summary: "init".into(),
                behind: 2,
            }),
            others: Vec::new(),
        }
    }

    fn fg_of(line: &Line<'_>, text: &str) -> Option<Color> {
        line.spans.iter().find(|s| s.content == text)?.style.fg
    }

    #[test]
    fn the_base_closes_the_stacks_under_the_upstream_lag() {
        let tree = Tree::new(content());

        let lines: Vec<String> = tree
            .lines(NOW, &Theme::default())
            .iter()
            .map(Line::to_string)
            .collect();

        assert_eq!(
            lines[lines.len() - 2..],
            ["┊ upstream +2", "┴ 3917b63 (common base) 2026-10-14 init"]
        );
        assert!(!tree.is_selectable(lines.len() - 1));
    }

    #[test]
    fn no_upstream_row_without_lag() {
        let mut content = content();
        content.base.as_mut().unwrap().behind = 0;

        let lines = Tree::new(content).lines(NOW, &Theme::default());

        assert!(
            !lines
                .iter()
                .any(|line| line.to_string().contains("upstream"))
        );
    }

    #[test]
    fn colors_come_from_the_theme() {
        let theme = Theme {
            separator: (1, 1, 1),
            status_added: (2, 2, 2),
            status_typechange: (4, 4, 4),
            ..Theme::default()
        };
        let lines = Tree::new(content()).lines(NOW, &theme);
        let rgb = |(r, g, b)| Some(Color::Rgb(r, g, b));

        assert_eq!(fg_of(&lines[0], "to"), rgb(theme.separator));
        assert_eq!(fg_of(&lines[0], "top-feature"), rgb(theme.status_added));
        assert_eq!(fg_of(&lines[1], "swuu"), rgb(theme.status_typechange));
    }
}
