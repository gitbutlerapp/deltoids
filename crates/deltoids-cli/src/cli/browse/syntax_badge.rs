//! The language badge on the diff pane's border, which tells whether
//! deltoids understands a file's syntax. A language with tree-sitter scope support shows its glyph and
//! name; a file syntect highlights without scope support shows a muted
//! "no scope context" marker; plain text shows nothing.

use ratatui::{style::Style, text::Span};

use deltoids::render_tui::rgb_to_color;
use deltoids::{Language, Theme};

use crate::sidebar::{IconMode, file_icon};

/// Syntect's syntax name for plain text, which has nothing to support.
const PLAIN_TEXT: &str = "Plain Text";

#[derive(Debug, Clone)]
pub(in crate::cli::browse) struct SyntaxBadge {
    spans: Vec<Span<'static>>,
}

impl SyntaxBadge {
    /// The badge for a file with tree-sitter `language` and syntect
    /// `highlight` name at `path`. `None` for plain text (neither).
    pub(in crate::cli::browse) fn new(
        language: Option<Language>,
        highlight: Option<&str>,
        path: &str,
        icons: IconMode,
        theme: &Theme,
    ) -> Option<Self> {
        let muted = Style::default().fg(rgb_to_color(theme.muted));
        let spans = match (language, highlight) {
            (Some(language), _) => {
                let mut spans = Vec::new();
                if icons == IconMode::On {
                    let icon = file_icon(path);
                    spans.push(Span::styled(
                        format!("{} ", icon.glyph),
                        Style::default().fg(rgb_to_color(icon.color)),
                    ));
                }
                spans.push(Span::styled(language.name(), muted));
                spans
            }
            (None, Some(highlight)) if highlight != PLAIN_TEXT => {
                vec![Span::styled(
                    format!("{highlight} · no scope context"),
                    muted,
                )]
            }
            (None, _) => return None,
        };
        Some(SyntaxBadge { spans })
    }

    /// The badge as styled spans.
    pub(in crate::cli::browse) fn spans(&self) -> &[Span<'static>] {
        &self.spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;

    fn badge_text(language: Option<Language>, highlight: Option<&str>, icons: IconMode) -> String {
        let theme = Theme::default();
        let badge =
            SyntaxBadge::new(language, highlight, "src/Main.kt", icons, &theme).expect("badge");
        Line::from(badge.spans().to_vec()).to_string()
    }

    #[test]
    fn supported_language_shows_glyph_and_name() {
        let text = badge_text(Some(Language::Kotlin), Some("Kotlin"), IconMode::On);
        assert_eq!(text, "\u{e634} Kotlin");
    }

    #[test]
    fn supported_language_without_icons_shows_name_only() {
        let text = badge_text(Some(Language::Kotlin), Some("Kotlin"), IconMode::Off);
        assert_eq!(text, "Kotlin");
    }

    #[test]
    fn unsupported_syntax_shows_muted_marker() {
        let theme = Theme::default();
        let badge = SyntaxBadge::new(None, Some("Dockerfile"), "Dockerfile", IconMode::On, &theme)
            .expect("badge");
        let span = badge.spans().last().expect("badge span");
        assert_eq!(span.content, "Dockerfile · no scope context");
        assert_eq!(span.style.fg, Some(rgb_to_color(theme.muted)));
    }

    #[test]
    fn plain_text_has_no_badge() {
        let theme = Theme::default();
        assert!(SyntaxBadge::new(None, None, "notes", IconMode::On, &theme).is_none());
        assert!(
            SyntaxBadge::new(None, Some("Plain Text"), "notes.txt", IconMode::On, &theme).is_none()
        );
    }
}
