//! Selection and scroll of one list pane.

use std::ops::Range;

use crossterm::event::KeyCode;

/// The selected row of a list and the first row on screen.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cursor {
    pub(super) selected: usize,
    scroll: usize,
}

impl Cursor {
    /// Move for a navigation key over a list of `len` rows shown
    /// `height` at a time. Other keys are ignored.
    pub(super) fn handle_key(&mut self, key: KeyCode, len: usize, height: usize) {
        let last = len.saturating_sub(1);
        let page = height.max(1);
        self.selected = match key {
            KeyCode::Char('j') | KeyCode::Down => self.selected.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => self.selected.saturating_sub(1),
            KeyCode::PageDown => self.selected.saturating_add(page),
            KeyCode::PageUp => self.selected.saturating_sub(page),
            KeyCode::Char('g') | KeyCode::Home => 0,
            KeyCode::Char('G') | KeyCode::End => last,
            _ => return,
        }
        .min(last);
    }

    /// Select the row `offset` rows below the top of the pane, if any,
    /// and return it.
    pub(super) fn click(&mut self, offset: usize, len: usize) -> Option<usize> {
        let index = self.scroll + offset;
        (index < len).then(|| {
            self.selected = index;
            index
        })
    }

    /// The rows on screen in a pane `height` rows tall, scrolled just
    /// enough to keep the selection in view.
    pub(super) fn visible(&mut self, len: usize, height: usize) -> Range<usize> {
        let height = height.max(1);
        self.selected = self.selected.min(len.saturating_sub(1));
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
        self.scroll = self.scroll.min(len.saturating_sub(height));
        self.scroll..(self.scroll + height).min(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_stay_inside_the_list() {
        let mut cursor = Cursor::default();
        cursor.handle_key(KeyCode::Char('k'), 3, 10);
        assert_eq!(cursor.selected, 0);
        cursor.handle_key(KeyCode::Char('G'), 3, 10);
        assert_eq!(cursor.selected, 2);
        cursor.handle_key(KeyCode::Char('j'), 3, 10);
        assert_eq!(cursor.selected, 2);
        cursor.handle_key(KeyCode::PageUp, 3, 10);
        assert_eq!(cursor.selected, 0);
    }

    #[test]
    fn the_view_scrolls_to_keep_the_selection_on_screen() {
        let mut cursor = Cursor::default();
        cursor.handle_key(KeyCode::End, 10, 4);
        assert_eq!(cursor.visible(10, 4), 6..10);
        cursor.handle_key(KeyCode::Home, 10, 4);
        assert_eq!(cursor.visible(10, 4), 0..4);
    }

    #[test]
    fn a_click_selects_relative_to_the_scroll() {
        let mut cursor = Cursor::default();
        cursor.handle_key(KeyCode::End, 10, 4);
        cursor.visible(10, 4);
        cursor.click(1, 10);
        assert_eq!(cursor.selected, 7);
        cursor.click(9, 10);
        assert_eq!(cursor.selected, 7);
    }
}
