//! The root mode the shell drives: the working tree, or the history
//! opened over it.
//!
//! `b` opens [`History`] on the working tree's repository; history
//! closes itself when the user steps back out of its branches. The
//! working tree stays live underneath: its watcher and reloads keep
//! running, so closing history lands on the current diff.

use std::path::PathBuf;

use crossterm::event::{KeyCode, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;

use deltoids::{ChangeLayout, Theme, git};

use super::files::FilesMode;
use super::history::History;
use super::mode::{AppCommand, BackgroundWork, DrawBudget, Mode, Viewport};
use super::watch::ChangeReceiver;

pub(super) struct Workspace {
    files: FilesMode,
    history: Option<History>,
}

impl Workspace {
    pub(super) fn new(files: FilesMode) -> Self {
        Self {
            files,
            history: None,
        }
    }

    /// The mode on screen.
    fn active(&mut self) -> &mut dyn Mode {
        match self.history.as_mut() {
            Some(history) => history,
            None => &mut self.files,
        }
    }

    fn active_ref(&self) -> &dyn Mode {
        match self.history.as_ref() {
            Some(history) => history,
            None => &self.files,
        }
    }

    fn open_history(&mut self) {
        self.history = self
            .files
            .repo_dir()
            .and_then(|dir| git::Repo::discover_at(&dir))
            .map(History::open);
    }
}

impl Mode for Workspace {
    fn background(&mut self, active: bool) -> BackgroundWork {
        match self.history.as_mut() {
            Some(history) => {
                // Hidden working-tree work still lands (finished git
                // actions, judgments); its renders pause.
                self.files.background(false);
                history.background(active)
            }
            None => self.files.background(active),
        }
    }

    fn build(&mut self, theme: &Theme, diff_width: usize) {
        self.files.build(theme, diff_width);
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
        self.active()
            .draw(frame, left, right, layout, theme, budget);
    }

    fn handle_key(&mut self, key: KeyCode, height: usize) -> AppCommand {
        if self.history.is_none() && key == KeyCode::Char('b') && !self.captures_text_input() {
            self.open_history();
            return AppCommand::Continue;
        }
        let command = self.active().handle_key(key, height);
        if self.history.as_ref().is_some_and(History::is_closed) {
            self.history = None;
        }
        command
    }

    fn reserves_key(&self, key: KeyCode) -> bool {
        match &self.history {
            Some(history) => history.reserves_key(key),
            None => key == KeyCode::Char('b') || self.files.reserves_key(key),
        }
    }

    fn take_refresh_request(&mut self) -> bool {
        self.files.take_refresh_request()
    }

    fn captures_text_input(&self) -> bool {
        self.active_ref().captures_text_input()
    }

    fn report_copy(&mut self, result: Result<(), String>) {
        self.active().report_copy(result);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, height: usize) -> AppCommand {
        self.active().handle_mouse(mouse, height)
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

    /// Reloads the working tree, visible or not. Only a visible change
    /// asks for a frame.
    fn reload(&mut self, viewport: Viewport, theme: &Theme) -> Result<bool, String> {
        let changed = self.files.reload(viewport, theme)?;
        Ok(changed && self.history.is_none())
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.active_ref().selected_path()
    }
}

#[cfg(test)]
mod tests;
