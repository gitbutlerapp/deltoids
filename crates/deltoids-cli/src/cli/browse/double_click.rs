//! Double-click detection for the TUI panes.
//!
//! Terminals report each mouse press on its own; neither the mouse
//! protocol nor crossterm counts clicks. [`DoubleClick`] watches a pane's
//! mouse events and tells the second press of a double click apart from
//! a single click, the way lazygit does:
//!
//! - Two left presses pair when they land on the same row, at most one
//!   column apart, within [`THRESHOLD`] of each other.
//! - The double fires on the second press.
//! - A scroll or a press of another button between them breaks the pair;
//!   releases, drags, and moves do not.
//! - After a double, the next press starts over, so a triple click is a
//!   double and a single.
//!
//! This is the one place that owns click timing.

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

/// The longest gap between two presses of a double click.
const THRESHOLD: Duration = Duration::from_millis(500);

/// A left press, remembered until the next one.
#[derive(Debug, Clone, Copy)]
struct Press {
    row: u16,
    column: u16,
    at: Instant,
}

/// Tells a double click from two single ones. Feed it every mouse event
/// the pane receives.
#[derive(Debug, Default)]
pub(super) struct DoubleClick {
    last: Option<Press>,
}

impl DoubleClick {
    /// Whether `mouse` is the second left press of a double click.
    pub(super) fn press(&mut self, mouse: &MouseEvent) -> bool {
        self.press_at(mouse, Instant::now())
    }

    fn press_at(&mut self, mouse: &MouseEvent, now: Instant) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.last.is_some_and(|last| {
                    last.row == mouse.row
                        && last.column.abs_diff(mouse.column) <= 1
                        && now.saturating_duration_since(last.at) <= THRESHOLD
                });
                self.last = (!double).then_some(Press {
                    row: mouse.row,
                    column: mouse.column,
                    at: now,
                });
                double
            }
            MouseEventKind::Up(_) | MouseEventKind::Drag(_) | MouseEventKind::Moved => false,
            _ => {
                self.last = None;
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;

    fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn left(column: u16, row: u16) -> MouseEvent {
        event(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    fn ms(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn two_quick_presses_on_a_row_are_a_double_click() {
        let start = Instant::now();
        let mut clicks = DoubleClick::default();
        assert!(!clicks.press_at(&left(5, 3), start));
        let up = event(MouseEventKind::Up(MouseButton::Left), 5, 3);
        assert!(!clicks.press_at(&up, ms(start, 50)));
        assert!(clicks.press_at(&left(6, 3), ms(start, 500)));
    }

    #[test]
    fn a_slow_or_distant_second_press_is_a_single_click() {
        let start = Instant::now();
        let mut clicks = DoubleClick::default();
        clicks.press_at(&left(5, 3), start);
        assert!(!clicks.press_at(&left(5, 3), ms(start, 501)));
        assert!(!clicks.press_at(&left(5, 4), ms(start, 600)));
        assert!(!clicks.press_at(&left(7, 4), ms(start, 700)));
    }

    #[test]
    fn a_scroll_or_another_button_breaks_the_pair() {
        let start = Instant::now();
        let mut clicks = DoubleClick::default();
        clicks.press_at(&left(5, 3), start);
        clicks.press_at(&event(MouseEventKind::ScrollDown, 5, 3), ms(start, 10));
        assert!(!clicks.press_at(&left(5, 3), ms(start, 20)));

        let right = event(MouseEventKind::Down(MouseButton::Right), 5, 3);
        assert!(!clicks.press_at(&right, ms(start, 30)));
        assert!(!clicks.press_at(&left(5, 3), ms(start, 40)));
    }

    #[test]
    fn a_third_quick_press_starts_over() {
        let start = Instant::now();
        let mut clicks = DoubleClick::default();
        clicks.press_at(&left(5, 3), start);
        assert!(clicks.press_at(&left(5, 3), ms(start, 100)));
        assert!(!clicks.press_at(&left(5, 3), ms(start, 200)));
        assert!(clicks.press_at(&left(5, 3), ms(start, 300)));
    }
}
