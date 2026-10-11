//! History mode: local branches, the commits on each, and each commit's
//! diff, in the spirit of lazygit's branches → commits → commit files
//! drill-down.
//!
//! Three screens share the left column, each a step down from the last:
//!
//! - **Branches**: every local branch, the checked-out one first. The
//!   right pane shows the selected branch's log.
//! - **Commits**: the commits of the chosen branch, newest first, loaded
//!   a page at a time. The right pane previews the selected commit's
//!   diff.
//! - **Commit**: the chosen commit as a read-only [`FilesMode`]: its file
//!   tree on the left and its diff on the right, with every diff-pane
//!   feature (cursor, comments, hunk reshaping) but no staging.
//!
//! `Enter` steps down, `Esc` steps back up, and `Esc` on the branches
//! closes history ([`History::is_closed`]). Each step keeps its own
//! selection, so walking back up lands where the user left off.
//!
//! History is a static snapshot of the repository taken when it opens:
//! it never watches or reloads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use deltoids::render_tui::{
    pane_block_with_title_line, pane_border_color, pane_inner_height, render_pane_scrollbar,
    rgb_to_color,
};
use deltoids::{ChangeLayout, Theme, git};

use super::diff_scrollbar;
use super::files::{FilesMode, sidebar_title};
use super::mode::{AppCommand, BackgroundWork, DrawBudget, Mode, Viewport};
use super::watch::ChangeReceiver;

mod cursor;

use cursor::Cursor;

/// How many commits one page of the commit list loads.
const PAGE: usize = 200;

/// Which step of the drill-down the left column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Branches,
    Commits,
    Commit,
}

/// The commits of one branch, loaded a page at a time.
struct CommitList {
    branch: String,
    commits: Result<Vec<git::Commit>, String>,
    /// Whether the last load returned fewer commits than it asked for.
    exhausted: bool,
    cursor: Cursor,
}

/// The diff of one commit: the commit list's preview, and the whole
/// screen once the user steps into it.
struct CommitView {
    id: String,
    view: Result<FilesMode, String>,
}

pub(super) struct History {
    repo: git::Repo,
    screen: Screen,
    closed: bool,
    branches: Result<Vec<git::Branch>, String>,
    branch_cursor: Cursor,
    /// Each branch's log for the branches screen's right pane.
    logs: HashMap<String, Result<Vec<git::Commit>, String>>,
    commits: Option<CommitList>,
    commit: Option<CommitView>,
    /// Seconds since the epoch when history opened; commit ages count
    /// from here.
    now: i64,
    /// Width of the diff body a commit view renders at.
    diff_width: usize,
    left_rect: Rect,
    right_rect: Rect,
}

impl History {
    /// Open on the repository's local branches.
    pub(super) fn open(repo: git::Repo) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        Self {
            branches: repo.local_branches(),
            repo,
            screen: Screen::Branches,
            closed: false,
            branch_cursor: Cursor::default(),
            logs: HashMap::new(),
            commits: None,
            commit: None,
            now,
            diff_width: 0,
            left_rect: Rect::default(),
            right_rect: Rect::default(),
        }
    }

    /// Whether the user stepped back out of the branches screen.
    pub(super) fn is_closed(&self) -> bool {
        self.closed
    }

    fn branch_count(&self) -> usize {
        self.branches.as_ref().map_or(0, Vec::len)
    }

    fn selected_branch(&self) -> Option<&git::Branch> {
        self.branches
            .as_ref()
            .ok()?
            .get(self.branch_cursor.selected)
    }

    fn selected_commit(&self) -> Option<&git::Commit> {
        let list = self.commits.as_ref()?;
        list.commits.as_ref().ok()?.get(list.cursor.selected)
    }

    /// The commit view, while it shows the selected commit.
    fn current_commit_view(&mut self) -> Option<&mut FilesMode> {
        let id = self.selected_commit()?.id.clone();
        let commit = self.commit.as_mut()?;
        if commit.id != id {
            return None;
        }
        commit.view.as_mut().ok()
    }

    /// Build the selected commit's view unless it is already built.
    fn ensure_commit_view(&mut self, theme: &Theme) {
        let Some(selected) = self.selected_commit().cloned() else {
            self.commit = None;
            return;
        };
        if self.commit.as_ref().is_some_and(|c| c.id == selected.id) {
            return;
        }
        self.commit = Some(CommitView {
            view: FilesMode::commit(&self.repo, &selected, theme, self.diff_width),
            id: selected.id,
        });
    }

    fn open_commits(&mut self) {
        let Some(branch) = self.selected_branch().map(|b| b.name.clone()) else {
            return;
        };
        if self
            .commits
            .as_ref()
            .is_none_or(|list| list.branch != branch)
        {
            let commits = self.repo.commits(&branch, PAGE);
            self.commits = Some(CommitList {
                exhausted: commits.as_ref().map_or(true, |c| c.len() < PAGE),
                branch,
                commits,
                cursor: Cursor::default(),
            });
        }
        self.screen = Screen::Commits;
    }

    /// Load the next page once the selection reaches the last commit.
    fn load_more_commits(&mut self) {
        let Some(list) = self.commits.as_mut() else {
            return;
        };
        let Ok(commits) = &list.commits else {
            return;
        };
        if list.exhausted || list.cursor.selected + 1 < commits.len() {
            return;
        }
        let want = commits.len() + PAGE;
        if let Ok(more) = self.repo.commits(&list.branch, want) {
            list.exhausted = more.len() < want;
            list.commits = Ok(more);
        }
    }

    fn handle_list_key(&mut self, key: KeyCode, height: usize) {
        match self.screen {
            Screen::Branches => {
                let len = self.branch_count();
                self.branch_cursor.handle_key(key, len, height);
            }
            Screen::Commits => {
                if let Some(list) = self.commits.as_mut() {
                    let len = list.commits.as_ref().map_or(0, Vec::len);
                    list.cursor.handle_key(key, len, height);
                }
                self.load_more_commits();
            }
            Screen::Commit => {}
        }
    }

    /// The selected branch's log, loaded on first view.
    fn branch_log(&mut self) -> Option<&Result<Vec<git::Commit>, String>> {
        let name = self.selected_branch()?.name.clone();
        let repo = &self.repo;
        Some(
            self.logs
                .entry(name.clone())
                .or_insert_with(|| repo.commits(&name, PAGE)),
        )
    }

    fn draw_branches(&mut self, frame: &mut Frame<'_>, left: Rect, theme: &Theme) {
        let now = self.now;
        let rows = match &self.branches {
            Ok(branches) if branches.is_empty() => Err("No branches.".to_string()),
            Ok(branches) => Ok(branches.iter().map(|b| branch_row(b, now, theme)).collect()),
            Err(err) => Err(err.clone()),
        };
        let title = sidebar_title(pane_border_color(true, theme), "Branches", theme);
        draw_list(
            frame,
            left,
            title,
            None,
            rows,
            &mut self.branch_cursor,
            theme,
        );
    }

    fn draw_branch_log(&mut self, frame: &mut Frame<'_>, right: Rect, theme: &Theme) {
        let now = self.now;
        let lines = match self.branch_log() {
            Some(Ok(commits)) => commits.iter().map(|c| commit_row(c, now, theme)).collect(),
            Some(Err(err)) => vec![Line::from(err.clone())],
            None => Vec::new(),
        };
        let color = pane_border_color(false, theme);
        let block = pane_block_with_title_line(pane_title("Log", theme), color, None);
        frame.render_widget(Paragraph::new(lines).block(block), right);
    }

    fn draw_commits(&mut self, frame: &mut Frame<'_>, left: Rect, theme: &Theme) {
        let now = self.now;
        let footer = self.selected_commit().map(|c| commit_footer(c, now, theme));
        let Some(list) = self.commits.as_mut() else {
            return;
        };
        let rows = match &list.commits {
            Ok(commits) if commits.is_empty() => Err("No commits.".to_string()),
            Ok(commits) => Ok(commits.iter().map(|c| commit_row(c, now, theme)).collect()),
            Err(err) => Err(err.clone()),
        };
        let label = format!("Commits ({})", list.branch);
        let title = sidebar_title(pane_border_color(true, theme), &label, theme);
        draw_list(frame, left, title, footer, rows, &mut list.cursor, theme);
    }

    fn draw_preview(
        &mut self,
        frame: &mut Frame<'_>,
        right: Rect,
        layout: ChangeLayout,
        theme: &Theme,
        budget: DrawBudget,
    ) {
        // Building a commit's diff costs real time: wait for held keys to
        // settle before building the newly selected one.
        if budget == DrawBudget::Full {
            self.ensure_commit_view(theme);
        }
        let selected = self.selected_commit().map(|c| c.id.clone());
        let color = pane_border_color(false, theme);
        let message = |text: &str| {
            Paragraph::new(text.to_string())
                .style(Style::default().fg(rgb_to_color(theme.muted)))
                .block(pane_block_with_title_line(
                    pane_title("Diff", theme),
                    color,
                    None,
                ))
        };
        match self.commit.as_mut() {
            Some(commit) if Some(&commit.id) == selected.as_ref() => match &mut commit.view {
                Ok(view) => view.draw_diff(frame, right, layout, theme, budget, false),
                Err(err) => frame.render_widget(message(err), right),
            },
            _ if selected.is_none() => frame.render_widget(message(""), right),
            _ => frame.render_widget(message("Loading…"), right),
        }
    }

    /// Select the list row under a click in the left column.
    fn click_row(&mut self, row: u16) {
        let inner = self.left_rect.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        if row < inner.y || row >= inner.bottom() {
            return;
        }
        let offset = (row - inner.y) as usize;
        match self.screen {
            Screen::Branches => {
                let len = self.branch_count();
                self.branch_cursor.click(offset, len);
            }
            Screen::Commits => {
                if let Some(list) = self.commits.as_mut() {
                    let len = list.commits.as_ref().map_or(0, Vec::len);
                    list.cursor.click(offset, len);
                }
                self.load_more_commits();
            }
            Screen::Commit => {}
        }
    }
}

impl Mode for History {
    fn background(&mut self, active: bool) -> BackgroundWork {
        match self.current_commit_view() {
            Some(view) => view.background(active),
            None => BackgroundWork::default(),
        }
    }

    fn build(&mut self, _theme: &Theme, _diff_width: usize) {}

    fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        left: Rect,
        right: Rect,
        layout: ChangeLayout,
        theme: &Theme,
        budget: DrawBudget,
    ) {
        self.left_rect = left;
        self.right_rect = right;
        self.diff_width = diff_scrollbar::body_area(right).width as usize;
        match self.screen {
            Screen::Branches => {
                self.draw_branches(frame, left, theme);
                self.draw_branch_log(frame, right, theme);
            }
            Screen::Commits => {
                self.draw_commits(frame, left, theme);
                self.draw_preview(frame, right, layout, theme, budget);
            }
            Screen::Commit => {
                self.ensure_commit_view(theme);
                match self.commit.as_mut().map(|c| &mut c.view) {
                    Some(Ok(view)) => view.draw(frame, left, right, layout, theme, budget),
                    Some(Err(err)) => {
                        let muted = Style::default().fg(rgb_to_color(theme.muted));
                        let color = pane_border_color(true, theme);
                        let block = pane_block_with_title_line(
                            sidebar_title(color, "Commit", theme),
                            color,
                            None,
                        );
                        frame.render_widget(
                            Paragraph::new(err.clone()).style(muted).block(block),
                            left,
                        );
                    }
                    None => {}
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyCode, height: usize) -> AppCommand {
        match (self.screen, key) {
            (Screen::Commit, _) if self.captures_text_input() => self
                .current_commit_view()
                .map_or(AppCommand::Continue, |view| view.handle_key(key, height)),
            (Screen::Commit, KeyCode::Esc) => {
                self.screen = Screen::Commits;
                AppCommand::Continue
            }
            (Screen::Commit, _) => self
                .current_commit_view()
                .map_or(AppCommand::Continue, |view| view.handle_key(key, height)),
            (Screen::Commits, KeyCode::Esc) => {
                self.screen = Screen::Branches;
                AppCommand::Continue
            }
            // The next draw builds the commit's view if the preview has
            // not yet.
            (Screen::Commits, KeyCode::Enter) => {
                if self.selected_commit().is_some() {
                    self.screen = Screen::Commit;
                }
                AppCommand::Continue
            }
            // Shift+J/K scroll the preview, as they scroll the diff in
            // the working-tree view.
            (Screen::Commits, KeyCode::Char('J' | 'K')) => self
                .current_commit_view()
                .map_or(AppCommand::Continue, |view| view.handle_key(key, height)),
            (Screen::Branches, KeyCode::Esc) => {
                self.closed = true;
                AppCommand::Continue
            }
            (Screen::Branches, KeyCode::Enter) => {
                self.open_commits();
                AppCommand::Continue
            }
            _ => {
                self.handle_list_key(key, height);
                AppCommand::Continue
            }
        }
    }

    fn reserves_key(&self, key: KeyCode) -> bool {
        self.screen == Screen::Commit
            && self
                .commit
                .as_ref()
                .and_then(|c| c.view.as_ref().ok())
                .is_some_and(|view| view.reserves_key(key))
    }

    fn captures_text_input(&self) -> bool {
        self.screen == Screen::Commit
            && self
                .commit
                .as_ref()
                .and_then(|c| c.view.as_ref().ok())
                .is_some_and(FilesMode::captures_text_input)
    }

    fn report_copy(&mut self, result: Result<(), String>) {
        if let Some(view) = self.current_commit_view() {
            view.report_copy(result);
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, height: usize) -> AppCommand {
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let to_commit = match self.screen {
            Screen::Commit => true,
            Screen::Commits => self.right_rect.contains(position),
            Screen::Branches => false,
        };
        if to_commit {
            return self
                .current_commit_view()
                .map_or(AppCommand::Continue, |view| {
                    view.handle_mouse(mouse, height)
                });
        }
        if !self.left_rect.contains(position) {
            return AppCommand::Continue;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click_row(mouse.row),
            MouseEventKind::ScrollDown => self.handle_list_key(KeyCode::Down, height),
            MouseEventKind::ScrollUp => self.handle_list_key(KeyCode::Up, height),
            _ => {}
        }
        AppCommand::Continue
    }

    fn watch(&mut self) -> Result<Option<ChangeReceiver>, String> {
        Ok(None)
    }

    fn should_reload(&self, _paths: &[PathBuf]) -> bool {
        false
    }

    fn reload(&mut self, _viewport: Viewport, _theme: &Theme) -> Result<bool, String> {
        Ok(false)
    }

    fn selected_path(&self) -> Option<PathBuf> {
        None
    }
}

/// Draw a list pane: `rows`, or a muted message in their place, with the
/// cursor's row highlighted across the full width.
fn draw_list(
    frame: &mut Frame<'_>,
    area: Rect,
    title: Line<'static>,
    footer: Option<Line<'static>>,
    rows: Result<Vec<Line<'static>>, String>,
    cursor: &mut Cursor,
    theme: &Theme,
) {
    let color = pane_border_color(true, theme);
    let block = pane_block_with_title_line(title, color, footer);
    let rows = match rows {
        Ok(rows) => rows,
        Err(message) => {
            let muted = Style::default().fg(rgb_to_color(theme.muted));
            frame.render_widget(Paragraph::new(message).style(muted).block(block), area);
            return;
        }
    };
    let height = pane_inner_height(area);
    let width = area.width.saturating_sub(2) as usize;
    let visible = cursor.visible(rows.len(), height);
    let selected_bg = Style::default().bg(rgb_to_color(theme.selection_bg));
    let lines: Vec<Line<'static>> = rows[visible.clone()]
        .iter()
        .zip(visible.clone())
        .map(|(row, index)| {
            if index != cursor.selected {
                return row.clone();
            }
            let mut row = row.clone().patch_style(selected_bg);
            let pad = width.saturating_sub(row.width());
            row.spans.push(Span::styled(" ".repeat(pad), selected_bg));
            row
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
    render_pane_scrollbar(
        frame,
        area,
        rows.len(),
        cursor.selected,
        height,
        true,
        theme,
    );
}

/// A plain pane title in the sidebar's `─Label─` style, without a badge.
fn pane_title(label: &str, theme: &Theme) -> Line<'static> {
    let rule = Style::default().fg(rgb_to_color(theme.border));
    Line::from(vec![
        Span::styled("─", rule),
        Span::styled(label.to_string(), rule),
        Span::styled("─", rule),
    ])
}

/// `  2d * main`: the tip's age, a star on the checked-out branch.
fn branch_row(branch: &git::Branch, now: i64, theme: &Theme) -> Line<'static> {
    let muted = Style::default().fg(rgb_to_color(theme.muted));
    let (marker, name) = if branch.head {
        (
            "* ",
            Style::default()
                .fg(rgb_to_color(theme.status_added))
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("  ", Style::default())
    };
    Line::from(vec![
        Span::styled(format!("{:>3} ", age(now, branch.time)), muted),
        Span::styled(marker, name),
        Span::styled(branch.name.clone(), name),
    ])
}

/// `abc1234  2d subject`.
fn commit_row(commit: &git::Commit, now: i64, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{} ", commit.short_id),
            Style::default().fg(rgb_to_color(theme.status_modified)),
        ),
        Span::styled(
            format!("{:>3} ", age(now, commit.time)),
            Style::default().fg(rgb_to_color(theme.muted)),
        ),
        Span::raw(commit.summary.clone()),
    ])
}

/// `Ada Lovelace · 2d ago`, under the commit list.
fn commit_footer(commit: &git::Commit, now: i64, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!(" {} · {} ago ", commit.author, age(now, commit.time)),
        Style::default().fg(rgb_to_color(theme.muted)),
    ))
}

/// How long ago `time` was at `now`, in the largest whole unit: `45s`,
/// `3m`, `2h`, `4d`, `2w`, `5M`, `1y`. Times after `now` read as `0s`.
pub(super) fn age(now: i64, time: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let seconds = (now - time).max(0);
    let days = seconds / DAY;
    match seconds {
        s if s < MINUTE => format!("{s}s"),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        _ if days < 7 => format!("{days}d"),
        _ if days < 30 => format!("{}w", days / 7),
        _ if days < 365 => format!("{}M", days / 30),
        _ => format!("{}y", days / 365),
    }
}

#[cfg(test)]
mod tests;
