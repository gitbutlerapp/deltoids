//! The `Panel` seam: one numbered side panel of the lazygit layout, plus
//! the main view it drives while it is the current panel.
//!
//! [`super::workspace::Workspace`] stacks the side panels in the left
//! column, gives the current one the main view on the right, and decides
//! which half has focus. A panel owns everything else: its state, its
//! keys, its mouse hit-testing, and what both halves render.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use deltoids::render_tui::rgb_to_color;
use deltoids::{ChangeLayout, Theme};

use super::mode::{AppCommand, BackgroundWork, DrawBudget};
use crate::sidebar::{IconMode, filter_icon};

pub(super) trait Panel {
    /// Draw the side panel into `area`, titled with its `number`.
    /// `focused` when the side panel has key focus.
    fn draw_side(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        number: char,
        focused: bool,
        theme: &Theme,
    );

    /// Draw the main view this panel drives into `area`. `focused` when
    /// the main view has key focus.
    fn draw_main(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        focused: bool,
        layout: ChangeLayout,
        theme: &Theme,
        budget: DrawBudget,
    );

    /// Send keys to the main view (`true`) or to the side panel.
    fn focus_main(&mut self, main: bool);

    /// Whether keys go to the main view. A panel may move focus there on
    /// its own, as `Enter` on a file does.
    fn main_focused(&self) -> bool;

    /// Handle a key for whichever half has focus. `height` is that
    /// half's inner height.
    fn handle_key(&mut self, key: KeyCode, height: usize) -> AppCommand;

    /// Handle a key with its modifiers, as
    /// [`super::mode::Mode::handle_key_event`].
    fn handle_key_event(&mut self, key: KeyEvent, height: usize) -> AppCommand {
        self.handle_key(key.code, height)
    }

    /// Drop transient state of the panel's own on `Esc` (a pending
    /// lookup, an open list) and say whether there was any. When there
    /// was none, `Esc` keeps its workspace meaning.
    fn escape(&mut self) -> bool {
        false
    }

    /// Handle a mouse event over either half, hit-tested against the
    /// rects of the last draw. `height` is the inner height of the half
    /// under the pointer.
    fn handle_mouse(&mut self, mouse: MouseEvent, height: usize) -> AppCommand;

    /// Collect background render work, as [`super::mode::Mode::background`].
    fn background(&mut self, _active: bool) -> BackgroundWork {
        BackgroundWork::default()
    }

    /// Keys this panel binds over configured custom commands.
    fn reserves_key(&self, _key: KeyCode) -> bool {
        false
    }

    /// Whether a modal editor owns every key.
    fn captures_text_input(&self) -> bool {
        false
    }

    /// The outcome of a clipboard copy this panel asked for.
    fn report_copy(&mut self, _result: Result<(), String>) {}

    /// The file on disk the selection points at, for custom commands.
    fn selected_path(&self) -> Option<PathBuf> {
        None
    }
}

/// A pane's top border title in lazygit's style: `─[2]─Files─`. The
/// badge and the label use the bold accent; the `─` rules use
/// `rule_color`, the pane's own border colour, so the title stays
/// continuous with the border (accent when focused, plain otherwise).
pub(crate) fn panel_title(
    rule_color: Color,
    number: char,
    label: &str,
    theme: &Theme,
) -> Line<'static> {
    let rule = Style::default().fg(rule_color);
    let accent = Style::default()
        .fg(rgb_to_color(theme.border_active))
        .add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled("─", rule),
        Span::styled(format!("[{number}]"), accent),
        Span::styled("─", rule),
        Span::styled(label.to_string(), accent),
        Span::styled("─", rule),
    ])
}

/// Paint the filter glyph at the right end of `area`'s top border, one
/// rule in from the corner, to say a filter hides some changes.
pub(crate) fn draw_filter_flag(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    let glyph = filter_icon(IconMode::from_env());
    let width = glyph.width() as u16;
    if area.width < width + 4 || area.height == 0 {
        return;
    }
    let flag = Rect {
        x: area.right() - 2 - width,
        y: area.y,
        width,
        height: 1,
    };
    let style = Style::default()
        .fg(rgb_to_color(theme.border_active))
        .add_modifier(Modifier::BOLD);
    frame.render_widget(Paragraph::new(Span::styled(glyph, style)), flag);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_reads_badge_and_label_in_the_accent_between_border_rules() {
        let theme = Theme::default();
        let rule_color = rgb_to_color(theme.border);
        let line = panel_title(rule_color, '2', "Files", &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "─[2]─Files─");
        let files = line.spans.iter().find(|s| s.content == "Files").unwrap();
        assert_eq!(files.style.fg, Some(rgb_to_color(theme.border_active)));
        assert!(files.style.add_modifier.contains(Modifier::BOLD));
        let rule = line.spans.iter().find(|s| s.content == "─").unwrap();
        assert_eq!(rule.style.fg, Some(rule_color));
    }
}
