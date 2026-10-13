use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use deltoids::Theme;
use deltoids::render_tui::{pane_block, pane_border_color, rgb_to_color};

use crate::cli::browse::vcs::{DiscardChoice, PendingDiscard};

/// The discard menu. Until the selection is read it shows the
/// repository's choices, none available.
#[derive(Debug)]
pub(super) enum DiscardMenu {
    Checking(Vec<DiscardChoice>),
    Ready(PendingDiscard),
    Unavailable(Vec<DiscardChoice>, String),
}

impl DiscardMenu {
    /// The menu's rows above "Cancel".
    pub(super) fn choices(&self) -> &[DiscardChoice] {
        match self {
            Self::Checking(choices) | Self::Unavailable(choices, _) => choices,
            Self::Ready(pending) => pending.choices(),
        }
    }
}

pub(super) fn draw(
    frame: &mut ratatui::Frame<'_>,
    menu: &DiscardMenu,
    selected: usize,
    theme: &Theme,
) {
    let area = frame.area();
    let width = area.width.min(80);
    let height = area.height.min(10);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let label = match menu {
        DiscardMenu::Ready(pending) => pending.label(),
        _ => String::new(),
    };
    let mut lines = vec![Line::from(label), Line::from("")];
    let choices = menu.choices();
    let labels = choices.iter().map(|choice| choice.label).chain(["Cancel"]);
    for (index, label) in labels.enumerate() {
        let mut style = Style::default();
        if choices.get(index).is_some_and(|choice| !choice.available) {
            style = style
                .fg(rgb_to_color(theme.muted))
                .add_modifier(Modifier::CROSSED_OUT);
        }
        if index == selected {
            style = style.bg(rgb_to_color(theme.selection_bg));
        }
        lines.push(Line::from(Span::styled(
            format!("{} {label}", if index == selected { "›" } else { " " }),
            style,
        )));
    }
    lines.push(Line::from(""));
    let description = match menu {
        DiscardMenu::Checking(_) => "Checking discard…",
        DiscardMenu::Unavailable(_, reason) => reason,
        DiscardMenu::Ready(_) if !choices.iter().any(|choice| choice.available) => {
            "No changes to discard"
        }
        DiscardMenu::Ready(_) => choices
            .get(selected)
            .map_or("", |choice| choice.description),
    };
    lines.push(Line::from(description));
    frame.render_widget(Clear, popup);
    let block = pane_block("─Discard changes─", pane_border_color(true, theme));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}
