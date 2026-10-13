//! The branches panel, in the spirit of lazygit's.
//!
//! [`History`] lists the branches [`Vcs::branches`] gives (local
//! branches, or a GitButler workspace's branches), the current ones first; the
//! main view shows the selected branch's log. `Enter` lists the branch's
//! commits in the same panel, newest first and loaded a page at a time,
//! and the main view previews the selected commit's diff. `Enter` again
//! opens the commit in the panel as a read-only [`FilesMode`]: its file
//! tree in the panel and its diff in the main view, with every diff-pane
//! feature (cursor, comments, hunk reshaping) but no staging. `Esc` steps
//! back up to the branches; each step keeps its selection.
//!
//! The lists are a snapshot; [`History::refresh`] re-reads them and keeps
//! each selection on the same branch or commit.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Margin, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use deltoids::render_tui::{
    pane_block_with_title_line, pane_border_color, pane_inner_height, render_pane_scrollbar,
    rgb_to_color,
};
use deltoids::{ChangeLayout, Theme, git};

use super::diff_scrollbar;
use super::files::FilesMode;
use super::mode::{AppCommand, BackgroundWork, DrawBudget};
use super::panel::{Panel, panel_title};
use super::vcs::{Branch, Vcs};

mod cursor;

use cursor::Cursor;

/// How many commits one page of a commit list loads.
const PAGE: usize = 200;

/// Which step of the drill-down the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Branches,
    Commits,
    Commit,
}

/// The commits of one branch, loaded a page at a time.
struct CommitList {
    branch: Branch,
    label: String,
    commits: Result<Vec<git::Commit>, String>,
    /// Whether the last load returned fewer commits than it asked for.
    exhausted: bool,
    cursor: Cursor,
}

impl CommitList {
    fn load(vcs: &Vcs, branch: Branch, label: String) -> Self {
        let commits = vcs.commits(&branch, PAGE);
        Self {
            exhausted: commits.as_ref().map_or(true, |c| c.len() < PAGE),
            branch,
            label,
            commits,
            cursor: Cursor::default(),
        }
    }

    fn len(&self) -> usize {
        self.commits.as_ref().map_or(0, Vec::len)
    }

    fn selected(&self) -> Option<&git::Commit> {
        self.commits.as_ref().ok()?.get(self.cursor.selected)
    }

    /// Load the next page once the selection reaches the last commit.
    fn load_more(&mut self, vcs: &Vcs) {
        if self.exhausted || self.cursor.selected + 1 < self.len() {
            return;
        }
        let want = self.len() + PAGE;
        if let Ok(more) = vcs.commits(&self.branch, want) {
            self.exhausted = more.len() < want;
            self.commits = Ok(more);
        }
    }

    /// Re-read as many commits as are loaded, keeping the selection on
    /// the same commit when it is still there.
    fn refresh(&mut self, vcs: &Vcs) {
        let selected = self.selected().map(|c| c.id.clone());
        let want = self.len().max(PAGE);
        self.commits = vcs.commits(&self.branch, want);
        self.exhausted = self.len() < want;
        if let Some(index) = selected.and_then(|id| {
            let commits = self.commits.as_ref().ok()?;
            commits.iter().position(|c| c.id == id)
        }) {
            self.cursor.selected = index;
        }
    }
}

/// The diff of one commit: the commit list's preview, and the panel's
/// content once the user steps into it.
struct CommitView {
    id: String,
    view: Result<FilesMode, String>,
}

/// A list pane's frame: its title, footer, and whether it has focus.
struct Chrome {
    title: Line<'static>,
    footer: Option<Line<'static>>,
    focused: bool,
}

pub(super) struct History {
    vcs: Vcs,
    screen: Screen,
    main_focused: bool,
    /// The branch list's title.
    label: &'static str,
    branches: Result<Vec<Branch>, String>,
    branch_cursor: Cursor,
    /// Each branch's log for the branches screen's main view.
    logs: HashMap<String, Result<Vec<git::Commit>, String>>,
    /// First log line on screen.
    log_scroll: usize,
    commits: Option<CommitList>,
    commit: Option<CommitView>,
    /// Seconds since the epoch at the last read; commit ages count from
    /// here.
    now: i64,
    /// Width of the diff body a commit view renders at.
    diff_width: usize,
    side_rect: Rect,
    main_rect: Rect,
}

impl History {
    /// The branches panel.
    pub(super) fn branches(mut vcs: Vcs) -> Self {
        let list = vcs.branches();
        Self {
            label: list.label,
            branches: list.branches,
            vcs,
            screen: Screen::Branches,
            main_focused: false,
            branch_cursor: Cursor::default(),
            logs: HashMap::new(),
            log_scroll: 0,
            commits: None,
            commit: None,
            now: now(),
            diff_width: 0,
            side_rect: Rect::default(),
            main_rect: Rect::default(),
        }
    }

    /// Re-read branches and commits, keeping each selection on the same
    /// branch or commit when it still exists. An open commit stays open:
    /// a commit never changes.
    pub(super) fn refresh(&mut self) {
        self.now = now();
        let selected = self.selected_branch().map(|b| b.name.clone());
        let list = self.vcs.branches();
        self.label = list.label;
        self.branches = list.branches;
        if let Some(index) = selected.and_then(|name| {
            let branches = self.branches.as_ref().ok()?;
            branches.iter().position(|b| b.name == name)
        }) {
            self.branch_cursor.selected = index;
        }
        self.logs.clear();
        if let Some(list) = self.commits.as_mut() {
            list.refresh(&self.vcs);
        }
    }

    fn branch_count(&self) -> usize {
        self.branches.as_ref().map_or(0, Vec::len)
    }

    fn selected_branch(&self) -> Option<&Branch> {
        self.branches
            .as_ref()
            .ok()?
            .get(self.branch_cursor.selected)
    }

    fn selected_commit(&self) -> Option<&git::Commit> {
        self.commits.as_ref()?.selected()
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

    fn current_commit_view_ref(&self) -> Option<&FilesMode> {
        let id = &self.selected_commit()?.id;
        let commit = self.commit.as_ref()?;
        if &commit.id != id {
            return None;
        }
        commit.view.as_ref().ok()
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
            view: FilesMode::commit(self.vcs.repo(), &selected, theme, self.diff_width),
            id: selected.id,
        });
        self.sync_focus();
    }

    /// Point the commit view's own focus where this panel's keys go: its
    /// file tree only while the commit is open with the panel focused.
    fn sync_focus(&mut self) {
        let main = self.main_focused || self.screen != Screen::Commit;
        if let Some(view) = self.current_commit_view() {
            view.focus_main(main);
        }
    }

    fn set_screen(&mut self, screen: Screen) {
        self.screen = screen;
        self.sync_focus();
    }

    fn open_branch_commits(&mut self) {
        let Some(branch) = self.selected_branch().cloned() else {
            return;
        };
        if self
            .commits
            .as_ref()
            .is_none_or(|list| list.branch.name != branch.name)
        {
            let label = format!("Commits ({})", branch.name);
            self.commits = Some(CommitList::load(&self.vcs, branch, label));
        }
        self.set_screen(Screen::Commits);
    }

    fn step_back(&mut self) {
        let up = match self.screen {
            Screen::Commit => Screen::Commits,
            Screen::Commits | Screen::Branches => Screen::Branches,
        };
        self.set_screen(up);
    }

    fn move_list(&mut self, key: KeyCode, height: usize) {
        match self.screen {
            Screen::Branches => {
                let len = self.branch_count();
                self.branch_cursor.handle_key(key, len, height);
                self.log_scroll = 0;
            }
            Screen::Commits => {
                if let Some(list) = self.commits.as_mut() {
                    let len = list.len();
                    list.cursor.handle_key(key, len, height);
                    list.load_more(&self.vcs);
                }
            }
            Screen::Commit => {}
        }
    }

    fn scroll_log(&mut self, key: KeyCode, height: usize) {
        let len = match self.branch_log() {
            Some(Ok(commits)) => commits.len(),
            _ => 0,
        };
        let last = len.saturating_sub(1);
        let page = height.max(1);
        self.log_scroll = match key {
            KeyCode::Char('j') | KeyCode::Down => self.log_scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.log_scroll.saturating_sub(1),
            KeyCode::PageDown => self.log_scroll.saturating_add(page),
            KeyCode::PageUp => self.log_scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => last,
            _ => return,
        }
        .min(last);
    }

    /// The selected branch's log, loaded on first view.
    fn branch_log(&mut self) -> Option<&Result<Vec<git::Commit>, String>> {
        let branch = self.selected_branch()?.clone();
        let vcs = &self.vcs;
        Some(
            self.logs
                .entry(branch.name.clone())
                .or_insert_with(|| vcs.commits(&branch, PAGE)),
        )
    }

    fn draw_branches(&mut self, frame: &mut Frame<'_>, area: Rect, chrome: Chrome, theme: &Theme) {
        let now = self.now;
        let rows = match &self.branches {
            Ok(branches) if branches.is_empty() => Err("No branches.".to_string()),
            Ok(branches) => Ok(branches.iter().map(|b| branch_row(b, now, theme)).collect()),
            Err(err) => Err(err.clone()),
        };
        draw_list(frame, area, chrome, rows, &mut self.branch_cursor, theme);
    }

    fn draw_commits(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        number: char,
        focused: bool,
        theme: &Theme,
    ) {
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
        let chrome = Chrome {
            title: panel_title(
                pane_border_color(focused, theme),
                number,
                &list.label,
                theme,
            ),
            footer,
            focused,
        };
        draw_list(frame, area, chrome, rows, &mut list.cursor, theme);
    }

    fn draw_branch_log(&mut self, frame: &mut Frame<'_>, area: Rect, focused: bool, theme: &Theme) {
        let now = self.now;
        let scroll = self.log_scroll;
        let lines = match self.branch_log() {
            Some(Ok(commits)) => commits
                .iter()
                .skip(scroll)
                .map(|c| commit_row(c, now, theme))
                .collect(),
            Some(Err(err)) => vec![Line::from(err.clone())],
            None => Vec::new(),
        };
        let color = pane_border_color(focused, theme);
        let block = pane_block_with_title_line(panel_title(color, '0', "Log", theme), color, None);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_commit_diff(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        focused: bool,
        layout: ChangeLayout,
        theme: &Theme,
        budget: DrawBudget,
    ) {
        // Building a commit's diff costs real time: in the list, wait for
        // held keys to settle before building the newly selected one.
        if budget == DrawBudget::Full || self.screen == Screen::Commit {
            self.ensure_commit_view(theme);
        }
        let selected = self.selected_commit().map(|c| c.id.clone());
        let color = pane_border_color(focused, theme);
        let message = |text: &str| {
            Paragraph::new(text.to_string())
                .style(Style::default().fg(rgb_to_color(theme.muted)))
                .block(pane_block_with_title_line(
                    panel_title(color, '0', "Diff", theme),
                    color,
                    None,
                ))
        };
        match self.commit.as_mut() {
            Some(commit) if Some(&commit.id) == selected.as_ref() => match &mut commit.view {
                Ok(view) => view.draw_main(frame, area, focused, layout, theme, budget),
                Err(err) => frame.render_widget(message(err), area),
            },
            _ if selected.is_none() => frame.render_widget(message(""), area),
            _ => frame.render_widget(message("Loading…"), area),
        }
    }

    /// Select the list row under a click in the side panel.
    fn click_row(&mut self, row: u16) {
        let inner = self.side_rect.inner(Margin {
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
                self.log_scroll = 0;
            }
            Screen::Commits => {
                if let Some(list) = self.commits.as_mut() {
                    let len = list.len();
                    list.cursor.click(offset, len);
                    list.load_more(&self.vcs);
                }
            }
            Screen::Commit => {}
        }
    }

    fn forward_key(&mut self, key: KeyCode, height: usize) -> AppCommand {
        self.current_commit_view()
            .map_or(AppCommand::Continue, |view| {
                Panel::handle_key(view, key, height)
            })
    }
}

impl Panel for History {
    fn draw_side(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        number: char,
        focused: bool,
        theme: &Theme,
    ) {
        self.side_rect = area;
        match self.screen {
            Screen::Branches => {
                let color = pane_border_color(focused, theme);
                let chrome = Chrome {
                    title: panel_title(color, number, self.label, theme),
                    footer: None,
                    focused,
                };
                self.draw_branches(frame, area, chrome, theme);
            }
            Screen::Commits => self.draw_commits(frame, area, number, focused, theme),
            Screen::Commit => {
                self.ensure_commit_view(theme);
                match self.commit.as_mut().map(|c| &mut c.view) {
                    Some(Ok(view)) => view.draw_side(frame, area, number, focused, theme),
                    Some(Err(err)) => {
                        let color = pane_border_color(focused, theme);
                        let title = panel_title(color, number, "Commit", theme);
                        let block = pane_block_with_title_line(title, color, None);
                        let muted = Style::default().fg(rgb_to_color(theme.muted));
                        let message = Paragraph::new(err.clone()).style(muted).block(block);
                        frame.render_widget(message, area);
                    }
                    None => {}
                }
            }
        }
    }

    fn draw_main(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        focused: bool,
        layout: ChangeLayout,
        theme: &Theme,
        budget: DrawBudget,
    ) {
        self.main_rect = area;
        self.diff_width = diff_scrollbar::body_area(area).width as usize;
        match self.screen {
            Screen::Branches => self.draw_branch_log(frame, area, focused, theme),
            Screen::Commits | Screen::Commit => {
                self.draw_commit_diff(frame, area, focused, layout, theme, budget);
            }
        }
    }

    fn focus_main(&mut self, main: bool) {
        self.main_focused = main;
        self.sync_focus();
    }

    fn main_focused(&self) -> bool {
        self.main_focused
    }

    fn handle_key(&mut self, key: KeyCode, height: usize) -> AppCommand {
        if self.captures_text_input() {
            return self.forward_key(key, height);
        }
        if self.main_focused {
            return match self.screen {
                Screen::Branches => {
                    self.scroll_log(key, height);
                    AppCommand::Continue
                }
                Screen::Commits | Screen::Commit => self.forward_key(key, height),
            };
        }
        match (self.screen, key) {
            (_, KeyCode::Esc) => self.step_back(),
            (Screen::Branches, KeyCode::Enter) => self.open_branch_commits(),
            // The next draw builds the commit's view if the preview has
            // not yet.
            (Screen::Commits, KeyCode::Enter) => {
                if self.selected_commit().is_some() {
                    self.set_screen(Screen::Commit);
                }
            }
            // Shift+J/K scroll the preview, as they scroll the diff from
            // the Files panel.
            (Screen::Commits, KeyCode::Char('J' | 'K')) => return self.forward_key(key, height),
            // An open commit's file tree; Enter on a file moves focus to
            // its diff.
            (Screen::Commit, _) => {
                let command = self.forward_key(key, height);
                if let Some(view) = self.current_commit_view() {
                    self.main_focused = view.main_focused();
                }
                return command;
            }
            _ => self.move_list(key, height),
        }
        AppCommand::Continue
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, height: usize) -> AppCommand {
        let position = Position::new(mouse.column, mouse.row);
        let in_main = self.main_rect.contains(position);
        let to_commit = match self.screen {
            Screen::Commit => true,
            Screen::Commits => in_main,
            Screen::Branches => false,
        };
        if to_commit {
            return self
                .current_commit_view()
                .map_or(AppCommand::Continue, |view| {
                    Panel::handle_mouse(view, mouse, height)
                });
        }
        let scroll = |history: &mut Self, key| {
            if in_main {
                history.scroll_log(key, height);
            } else {
                history.move_list(key, height);
            }
        };
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if !in_main => self.click_row(mouse.row),
            MouseEventKind::ScrollDown => scroll(self, KeyCode::Down),
            MouseEventKind::ScrollUp => scroll(self, KeyCode::Up),
            _ => {}
        }
        AppCommand::Continue
    }

    fn background(&mut self, active: bool) -> BackgroundWork {
        match self.current_commit_view() {
            Some(view) => Panel::background(view, active),
            None => BackgroundWork::default(),
        }
    }

    fn reserves_key(&self, key: KeyCode) -> bool {
        self.current_commit_view_ref()
            .filter(|_| self.screen == Screen::Commit || self.main_focused)
            .is_some_and(|view| Panel::reserves_key(view, key))
    }

    fn captures_text_input(&self) -> bool {
        self.current_commit_view_ref()
            .is_some_and(Panel::captures_text_input)
    }

    fn report_copy(&mut self, result: Result<(), String>) {
        if let Some(view) = self.current_commit_view() {
            Panel::report_copy(view, result);
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

/// Draw a list pane: `rows`, or a muted message in their place, with the
/// cursor's row highlighted across the full width.
fn draw_list(
    frame: &mut Frame<'_>,
    area: Rect,
    chrome: Chrome,
    rows: Result<Vec<Line<'static>>, String>,
    cursor: &mut Cursor,
    theme: &Theme,
) {
    let color = pane_border_color(chrome.focused, theme);
    let block = pane_block_with_title_line(chrome.title, color, chrome.footer);
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
        chrome.focused,
        theme,
    );
}

/// `  2d * main`: the tip's age, a star on the current branch.
fn branch_row(branch: &Branch, now: i64, theme: &Theme) -> Line<'static> {
    let muted = Style::default().fg(rgb_to_color(theme.muted));
    let (marker, name) = if branch.current {
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
fn age(now: i64, time: i64) -> String {
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
mod tests {
    use super::age;

    #[test]
    fn ages_read_in_the_largest_whole_unit() {
        assert_eq!(age(1_000, 1_000), "0s");
        assert_eq!(age(1_059, 1_000), "59s");
        assert_eq!(age(1_000 + 60, 1_000), "1m");
        assert_eq!(age(1_000 + 2 * 3600, 1_000), "2h");
        assert_eq!(age(1_000 + 3 * 86_400, 1_000), "3d");
        assert_eq!(age(1_000 + 14 * 86_400, 1_000), "2w");
        assert_eq!(age(1_000 + 60 * 86_400, 1_000), "2M");
        assert_eq!(age(1_000 + 800 * 86_400, 1_000), "2y");
        // A commit from the future (clock skew) reads as now.
        assert_eq!(age(1_000, 2_000), "0s");
    }
}
