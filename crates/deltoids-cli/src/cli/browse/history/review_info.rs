//! What the Files panel reviews, in place of the list: every commit of
//! the review, newest first, each with its short id and age, author, and
//! whole message wrapped to the pane, scrolled with the list keys.

use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use deltoids::render_tui::{
    pane_block_with_title_line, pane_border_color, pane_inner_height, render_pane_scrollbar,
    rgb_to_color,
};
use deltoids::{Theme, git};

use super::age;
use crate::cli::browse::files::Snapshot;
use crate::cli::browse::panel::panel_title;
use crate::cli::browse::text::wrap_text;

pub(super) struct ReviewInfo {
    snapshot: Snapshot,
    /// First line on screen.
    scroll: usize,
}

impl ReviewInfo {
    pub(super) fn new(snapshot: Snapshot) -> Self {
        Self {
            snapshot,
            scroll: 0,
        }
    }

    pub(super) fn shows(&self, snapshot: &Snapshot) -> bool {
        &self.snapshot == snapshot
    }

    /// Scroll for a navigation key, `height` rows shown at a time. Other
    /// keys are ignored. The next draw keeps the scroll inside the text.
    pub(super) fn handle_key(&mut self, key: KeyCode, height: usize) {
        let page = height.max(1);
        self.scroll = match key {
            KeyCode::Char('j') | KeyCode::Down => self.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll.saturating_add(page),
            KeyCode::PageUp => self.scroll.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => usize::MAX,
            _ => return,
        };
    }

    pub(super) fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        number: char,
        focused: bool,
        now: i64,
        theme: &Theme,
    ) {
        let width = area.width.saturating_sub(2) as usize;
        let lines: Vec<Line<'static>> = self
            .snapshot
            .commits()
            .iter()
            .enumerate()
            .flat_map(|(index, commit)| {
                let gap = (index > 0).then(Line::default);
                gap.into_iter()
                    .chain(commit_lines(commit, width, now, theme))
            })
            .collect();
        let height = pane_inner_height(area);
        self.scroll = self.scroll.min(lines.len().saturating_sub(height));
        let color = pane_border_color(focused, theme);
        let label = self.snapshot.label();
        let block =
            pane_block_with_title_line(panel_title(color, number, &label, theme), color, None);
        let visible: Vec<Line<'static>> = lines
            .iter()
            .skip(self.scroll)
            .take(height)
            .cloned()
            .collect();
        frame.render_widget(Paragraph::new(visible).block(block), area);
        render_pane_scrollbar(
            frame,
            area,
            lines.len(),
            self.scroll,
            height,
            focused,
            theme,
        );
    }
}

/// One commit's lines, each wrapped to `width`: short id and age,
/// author, a blank line, then the bold subject and the body.
fn commit_lines(commit: &git::Commit, width: usize, now: i64, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::default().fg(rgb_to_color(theme.muted));
    let mut lines = vec![Line::from(vec![
        Span::styled(
            commit.short_id.clone(),
            Style::default().fg(rgb_to_color(theme.status_modified)),
        ),
        Span::styled(format!(" · {} ago", age(now, commit.time)), muted),
    ])];
    let author = format!("{} <{}>", commit.author, commit.email);
    lines.extend(wrapped(&author, width, muted));
    lines.push(Line::default());
    let subject = Style::default().add_modifier(Modifier::BOLD);
    for (index, line) in commit.message.trim_end().lines().enumerate() {
        let style = if index == 0 {
            subject
        } else {
            Style::default()
        };
        if line.trim().is_empty() {
            lines.push(Line::default());
        } else {
            lines.extend(wrapped(line, width, style));
        }
    }
    lines
}

fn wrapped(text: &str, width: usize, style: Style) -> impl Iterator<Item = Line<'static>> {
    wrap_text(text, width.max(1))
        .into_iter()
        .map(move |part| Line::styled(part, style))
}
