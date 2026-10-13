//! The Status panel: `repo → branch` on one line, and a summary of the
//! repository and its checked-out commit in the main view.

use crossterm::event::{KeyCode, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use deltoids::render_tui::{pane_block_with_title_line, pane_border_color, rgb_to_color};
use deltoids::{ChangeLayout, Theme, git};

use crate::cli::browse::mode::{AppCommand, DrawBudget};
use crate::cli::browse::panel::{Panel, panel_title};

/// What the panel shows, read from the repository on each refresh.
#[derive(Default)]
pub(super) struct Status {
    repo: Option<git::Repo>,
    name: String,
    /// The checked-out branch, or `HEAD` at a short commit id when
    /// detached.
    branch: String,
    head: Option<git::Commit>,
}

impl Status {
    pub(super) fn open(repo: Option<git::Repo>) -> Self {
        let mut status = Self {
            repo,
            ..Self::default()
        };
        status.refresh();
        status
    }

    pub(super) fn refresh(&mut self) {
        let Some(repo) = &self.repo else {
            return;
        };
        let path = repo.workdir().unwrap_or(repo.git_dir());
        self.name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.head = repo.commits("HEAD", 1).ok().and_then(|mut c| c.pop());
        let checked_out = repo
            .local_branches()
            .ok()
            .and_then(|branches| branches.into_iter().find(|b| b.head));
        self.branch = match (checked_out, &self.head) {
            (Some(branch), _) => branch.name,
            (None, Some(head)) => format!("HEAD ({})", head.short_id),
            (None, None) => "no commits".to_string(),
        };
    }

    fn summary(&self, theme: &Theme) -> Vec<Line<'static>> {
        let Some(repo) = &self.repo else {
            return vec![Line::from("Not a git repository.")];
        };
        let label = |text: &str| {
            Span::styled(
                format!("{text:<12}"),
                Style::default().fg(rgb_to_color(theme.muted)),
            )
        };
        let path = repo.workdir().unwrap_or(repo.git_dir());
        let mut lines = vec![
            Line::from(vec![
                label("Repository"),
                Span::raw(path.display().to_string()),
            ]),
            Line::from(vec![label("Branch"), Span::raw(self.branch.clone())]),
        ];
        if let Some(head) = &self.head {
            lines.push(Line::from(vec![
                label("HEAD"),
                Span::styled(
                    format!("{} ", head.short_id),
                    Style::default().fg(rgb_to_color(theme.status_modified)),
                ),
                Span::raw(head.summary.clone()),
            ]));
            lines.push(Line::from(vec![
                label("Author"),
                Span::raw(format!("{} <{}>", head.author, head.email)),
            ]));
        }
        lines
    }
}

impl Panel for Status {
    fn draw_side(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        number: char,
        focused: bool,
        theme: &Theme,
    ) {
        let color = pane_border_color(focused, theme);
        let block =
            pane_block_with_title_line(panel_title(color, number, "Status", theme), color, None);
        let line = match &self.repo {
            Some(_) => Line::from(vec![
                Span::raw(format!("{} → ", self.name)),
                Span::styled(
                    self.branch.clone(),
                    Style::default().fg(rgb_to_color(theme.status_added)),
                ),
            ]),
            None => Line::from("Not a git repository."),
        };
        frame.render_widget(Paragraph::new(line).block(block), area);
    }

    fn draw_main(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        focused: bool,
        _layout: ChangeLayout,
        theme: &Theme,
        _budget: DrawBudget,
    ) {
        let color = pane_border_color(focused, theme);
        let block =
            pane_block_with_title_line(panel_title(color, '0', "Status", theme), color, None);
        frame.render_widget(Paragraph::new(self.summary(theme)).block(block), area);
    }

    fn focus_main(&mut self, _main: bool) {}

    fn handle_key(&mut self, _key: KeyCode, _height: usize) -> AppCommand {
        AppCommand::Continue
    }

    fn handle_mouse(&mut self, _mouse: MouseEvent, _height: usize) -> AppCommand {
        AppCommand::Continue
    }
}
