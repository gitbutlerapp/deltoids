//! The root mode the shell drives: lazygit's layout.
//!
//! The left column stacks two numbered side panels; the right pane is the
//! main view `[0]`, showing whatever the current side panel drives:
//!
//! 1. Files: the working-tree file tree; main shows its diff.
//! 2. Local branches: main shows the selected branch's log. Drills down
//!    into a branch's commits (main: the selected commit's diff) and a
//!    commit's files.
//!
//! Focus is on one side panel or on the main view. The current side
//! panel takes the larger share of the column. `1`/`2` focus a side
//! panel and `0` the main view; `Tab`/`Shift+Tab`, `h`/`l`, and the
//! arrows switch side panels. `Enter` on a file focuses its diff, and
//! `Esc` leaves the main view. Every other key, `Tab` in the main view
//! included, goes to the current panel, which owns both of its halves
//! through the [`Panel`] seam.
//!
//! The working tree is the one watched source. A reload rebuilds its
//! diff and re-reads the branches.

use std::path::PathBuf;

use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;

use deltoids::render_tui::{pane_block_with_title_line, pane_border_color, rgb_to_color};
use deltoids::{ChangeLayout, Theme, git};

use super::files::FilesMode;
use super::history::History;
use super::mode::{AppCommand, BackgroundWork, DrawBudget, Mode, Viewport};
use super::panel::{Panel, panel_title};
use super::watch::ChangeReceiver;

/// The side panels, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Files,
    Branches,
}

impl Side {
    const ALL: [Side; 2] = [Side::Files, Side::Branches];

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|&side| side == self)
            .expect("listed")
    }

    fn number(self) -> char {
        char::from(b'1' + self.index() as u8)
    }

    fn from_number(number: char) -> Option<Self> {
        let index = number.to_digit(10)?.checked_sub(1)?;
        Self::ALL.get(index as usize).copied()
    }

    /// The panel `step` places down the column, wrapping around.
    fn step(self, step: isize) -> Self {
        let len = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + step).rem_euclid(len) as usize]
    }
}

pub(super) struct Workspace {
    files: FilesMode,
    branches: Option<History>,
    /// The side panel that drives the main view.
    side: Side,
    main_focused: bool,
    side_rects: [Rect; 2],
    main_rect: Rect,
}

impl Workspace {
    pub(super) fn new(files: FilesMode) -> Self {
        let mut workspace = Self {
            files,
            branches: None,
            side: Side::Files,
            main_focused: false,
            side_rects: [Rect::default(); 2],
            main_rect: Rect::default(),
        };
        workspace.open_repository();
        workspace
    }

    /// Open the branches panel on the working tree's repository; without
    /// one it says so.
    fn open_repository(&mut self) {
        self.branches = self
            .files
            .repo_dir()
            .and_then(|dir| git::Repo::discover_at(&dir))
            .map(History::branches);
    }

    fn panel(&mut self, side: Side) -> Option<&mut dyn Panel> {
        match side {
            Side::Files => Some(&mut self.files),
            Side::Branches => self.branches.as_mut().map(|h| h as &mut dyn Panel),
        }
    }

    fn panel_ref(&self, side: Side) -> Option<&dyn Panel> {
        match side {
            Side::Files => Some(&self.files),
            Side::Branches => self.branches.as_ref().map(|h| h as &dyn Panel),
        }
    }

    /// Give `side` focus, on its side panel or its main view.
    fn focus(&mut self, side: Side, main: bool) {
        self.side = side;
        self.main_focused = main;
        if let Some(panel) = self.panel(side) {
            panel.focus_main(main);
        }
    }

    /// Hand a key to the current panel, then follow any focus move it
    /// made on its own.
    fn forward_key(&mut self, key: KeyCode, height: usize) -> AppCommand {
        let side = self.side;
        let Some(panel) = self.panel(side) else {
            return AppCommand::Continue;
        };
        let command = panel.handle_key(key, height);
        self.main_focused = panel.main_focused();
        command
    }

    /// Inner height of the half with focus; `main` is the main view's.
    fn focused_height(&self, main: usize) -> usize {
        if self.main_focused {
            main
        } else {
            inner_height(self.side_rects[self.side.index()])
        }
    }
}

impl Mode for Workspace {
    fn background(&mut self, active: bool) -> BackgroundWork {
        let mut work = BackgroundWork::default();
        for side in Side::ALL {
            if let Some(panel) = self.panel(side) {
                let panel_work = panel.background(active);
                work.changed |= panel_work.changed;
                work.pending |= panel_work.pending;
            }
        }
        work
    }

    fn build(&mut self, theme: &Theme, diff_width: usize) {
        Mode::build(&mut self.files, theme, diff_width);
        self.open_repository();
        self.focus(self.side, self.main_focused);
    }

    fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        left: Rect,
        right: Rect,
        layout: ChangeLayout,
        theme: &Theme,
        budget: DrawBudget,
    ) {
        self.side_rects = side_layout(left, self.side);
        self.main_rect = right;
        let (current, main_focused) = (self.side, self.main_focused);
        for side in Side::ALL {
            let area = self.side_rects[side.index()];
            let focused = side == current && !main_focused;
            match self.panel(side) {
                Some(panel) => panel.draw_side(frame, area, side.number(), focused, theme),
                None => {
                    draw_no_repository(frame, area, side.number(), "Local branches", focused, theme)
                }
            }
        }
        match self.panel(current) {
            Some(panel) => panel.draw_main(frame, right, main_focused, layout, theme, budget),
            None => draw_no_repository(frame, right, '0', "Log", main_focused, theme),
        }
    }

    fn handle_key(&mut self, key: KeyCode, height: usize) -> AppCommand {
        let height = self.focused_height(height);
        if self.captures_text_input() {
            return self.forward_key(key, height);
        }
        let side = self.side;
        match key {
            KeyCode::Char('0') => self.focus(side, true),
            KeyCode::Char(n) if Side::from_number(n).is_some() => {
                self.focus(Side::from_number(n).expect("checked by guard"), false);
            }
            KeyCode::Esc if self.main_focused => self.focus(side, false),
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right if !self.main_focused => {
                self.focus(side.step(1), false);
            }
            KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Left if !self.main_focused => {
                self.focus(side.step(-1), false);
            }
            _ => return self.forward_key(key, height),
        }
        AppCommand::Continue
    }

    fn reserves_key(&self, key: KeyCode) -> bool {
        matches!(key, KeyCode::Char('0'..='2' | 'h' | 'l'))
            || self
                .panel_ref(self.side)
                .is_some_and(|panel| panel.reserves_key(key))
    }

    fn take_refresh_request(&mut self) -> bool {
        self.files.take_refresh_request()
    }

    fn captures_text_input(&self) -> bool {
        self.panel_ref(self.side)
            .is_some_and(|panel| panel.captures_text_input())
    }

    fn report_copy(&mut self, result: Result<(), String>) {
        let side = self.side;
        if let Some(panel) = self.panel(side) {
            panel.report_copy(result);
        }
    }

    /// A click focuses the half under the pointer; the event then goes
    /// to the panel that owns that half. The wheel scrolls whatever is
    /// under the pointer without moving focus.
    fn handle_mouse(&mut self, mouse: MouseEvent, height: usize) -> AppCommand {
        if self.captures_text_input() {
            return AppCommand::Continue;
        }
        let position = Position::new(mouse.column, mouse.row);
        let click = matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left));
        let (side, panel_height) = if self.main_rect.contains(position) {
            if click {
                self.focus(self.side, true);
            }
            (self.side, height)
        } else if let Some(side) = Side::ALL
            .into_iter()
            .find(|side| self.side_rects[side.index()].contains(position))
        {
            if click {
                self.focus(side, false);
            }
            (side, inner_height(self.side_rects[side.index()]))
        } else {
            return AppCommand::Continue;
        };
        self.panel(side).map_or(AppCommand::Continue, |panel| {
            panel.handle_mouse(mouse, panel_height)
        })
    }

    fn watch(&mut self) -> Result<Option<ChangeReceiver>, String> {
        self.files.watch()
    }

    fn should_reload(&self, paths: &[PathBuf]) -> bool {
        self.files.should_reload(paths)
    }

    fn retry_reload(&self) -> bool {
        self.files.retry_reload()
    }

    /// Rebuild the working tree's diff and re-read the branches. Commits
    /// and branches move with the working tree (a commit empties the
    /// index), so this always asks for a frame.
    fn reload(&mut self, viewport: Viewport, theme: &Theme) -> Result<bool, String> {
        let changed = self.files.reload(viewport, theme)?;
        if let Some(branches) = &mut self.branches {
            branches.refresh();
        }
        Ok(changed || self.branches.is_some())
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.panel_ref(self.side)
            .and_then(|panel| panel.selected_path())
    }
}

/// Split the left column into the side panels, the current one taking
/// the larger share.
fn side_layout(area: Rect, current: Side) -> [Rect; 2] {
    let weight = |side| if side == current { 2 } else { 1 };
    let rects = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(weight(Side::Files)),
            Constraint::Fill(weight(Side::Branches)),
        ])
        .split(area);
    [rects[0], rects[1]]
}

fn inner_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

fn draw_no_repository(
    frame: &mut Frame<'_>,
    area: Rect,
    number: char,
    label: &str,
    focused: bool,
    theme: &Theme,
) {
    let color = pane_border_color(focused, theme);
    let block = pane_block_with_title_line(panel_title(color, number, label, theme), color, None);
    let muted = Style::default().fg(rgb_to_color(theme.muted));
    frame.render_widget(
        Paragraph::new("Not a git repository.")
            .style(muted)
            .block(block),
        area,
    );
}

#[cfg(test)]
mod tests;
