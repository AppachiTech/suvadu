use ratatui::widgets::TableState;

/// Shared pagination + row-selection state for the agent UI's paged tables
/// (dashboard, prompt list, prompt detail). Consolidates the page/selection
/// arithmetic that was previously duplicated across those screens: this type
/// only tracks `page`/`page_size`/`TableState`, not the underlying data, so
/// callers still own their own `Vec`/slice and pass its length in.
pub struct PagedTable {
    pub state: TableState,
    /// 1-based current page.
    pub page: usize,
    pub page_size: usize,
}

impl PagedTable {
    pub fn new(page_size: usize) -> Self {
        Self {
            state: TableState::default(),
            page: 1,
            page_size,
        }
    }

    pub fn total_pages(&self, total_len: usize) -> usize {
        total_len.div_ceil(self.page_size).max(1)
    }

    /// Start/end indices into the full list for the current page.
    pub fn bounds(&self, total_len: usize) -> (usize, usize) {
        let start = (self.page - 1) * self.page_size;
        if start >= total_len {
            (0, 0)
        } else {
            (start, (start + self.page_size).min(total_len))
        }
    }

    pub const fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    /// Reset to page 1 and select the first row (or clear selection if empty).
    /// Call after the underlying data changes shape (filter/reload).
    pub const fn reset(&mut self, total_len: usize) {
        self.page = 1;
        self.state
            .select(if total_len == 0 { None } else { Some(0) });
    }

    pub const fn move_up(&mut self) {
        if let Some(cur) = self.state.selected() {
            self.state.select(Some(cur.saturating_sub(1)));
        }
    }

    /// `page_len` is the length of the current page's slice (from `bounds`).
    pub fn move_down(&mut self, page_len: usize) {
        let max = page_len.saturating_sub(1);
        if let Some(cur) = self.state.selected() {
            self.state.select(Some(cur.saturating_add(1).min(max)));
        }
    }

    pub const fn select_first(&mut self, page_len: usize) {
        if page_len > 0 {
            self.state.select(Some(0));
        }
    }

    pub const fn select_last(&mut self, page_len: usize) {
        if page_len > 0 {
            self.state.select(Some(page_len - 1));
        }
    }

    /// Move to the previous page (if any) and select its first row.
    pub fn prev_page(&mut self, total_len: usize) {
        if self.page > 1 {
            self.page -= 1;
            let (start, end) = self.bounds(total_len);
            self.select_first(end - start);
        }
    }

    /// Move to the next page (if any) and select its first row.
    pub fn next_page(&mut self, total_len: usize) {
        if self.page < self.total_pages(total_len) {
            self.page += 1;
            let (start, end) = self.bounds(total_len);
            self.select_first(end - start);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_pages_empty_is_one() {
        let pt = PagedTable::new(10);
        assert_eq!(pt.total_pages(0), 1);
    }

    #[test]
    fn total_pages_exact_multiple() {
        let pt = PagedTable::new(10);
        assert_eq!(pt.total_pages(20), 2);
    }

    #[test]
    fn total_pages_rounds_up() {
        let pt = PagedTable::new(10);
        assert_eq!(pt.total_pages(21), 3);
    }

    #[test]
    fn bounds_first_page() {
        let pt = PagedTable::new(10);
        assert_eq!(pt.bounds(25), (0, 10));
    }

    #[test]
    fn bounds_last_partial_page() {
        let mut pt = PagedTable::new(10);
        pt.page = 3;
        assert_eq!(pt.bounds(25), (20, 25));
    }

    #[test]
    fn bounds_page_past_end_is_empty() {
        let mut pt = PagedTable::new(10);
        pt.page = 5;
        assert_eq!(pt.bounds(25), (0, 0));
    }

    #[test]
    fn reset_selects_first_row_when_nonempty() {
        let mut pt = PagedTable::new(10);
        pt.page = 3;
        pt.reset(5);
        assert_eq!(pt.page, 1);
        assert_eq!(pt.selected(), Some(0));
    }

    #[test]
    fn reset_clears_selection_when_empty() {
        let mut pt = PagedTable::new(10);
        pt.state.select(Some(2));
        pt.reset(0);
        assert_eq!(pt.selected(), None);
    }

    #[test]
    fn move_up_clamps_at_zero() {
        let mut pt = PagedTable::new(10);
        pt.state.select(Some(0));
        pt.move_up();
        assert_eq!(pt.selected(), Some(0));
    }

    #[test]
    fn move_up_decrements() {
        let mut pt = PagedTable::new(10);
        pt.state.select(Some(3));
        pt.move_up();
        assert_eq!(pt.selected(), Some(2));
    }

    #[test]
    fn move_down_clamps_at_page_len() {
        let mut pt = PagedTable::new(10);
        pt.state.select(Some(4));
        pt.move_down(5);
        assert_eq!(pt.selected(), Some(4));
    }

    #[test]
    fn move_down_increments() {
        let mut pt = PagedTable::new(10);
        pt.state.select(Some(2));
        pt.move_down(5);
        assert_eq!(pt.selected(), Some(3));
    }

    #[test]
    fn select_first_no_op_when_empty() {
        let mut pt = PagedTable::new(10);
        pt.state.select(Some(2));
        pt.select_first(0);
        assert_eq!(pt.selected(), Some(2));
    }

    #[test]
    fn select_last_picks_final_index() {
        let mut pt = PagedTable::new(10);
        pt.select_last(7);
        assert_eq!(pt.selected(), Some(6));
    }

    #[test]
    fn prev_page_no_op_on_first_page() {
        let mut pt = PagedTable::new(10);
        pt.prev_page(10);
        assert_eq!(pt.page, 1);
    }

    #[test]
    fn prev_page_decrements_and_selects_first() {
        let mut pt = PagedTable::new(10);
        pt.page = 2;
        pt.prev_page(10);
        assert_eq!(pt.page, 1);
        assert_eq!(pt.selected(), Some(0));
    }

    #[test]
    fn next_page_no_op_at_last_page() {
        let mut pt = PagedTable::new(10);
        pt.next_page(10);
        assert_eq!(pt.page, 1);
    }

    #[test]
    fn next_page_increments_and_selects_first() {
        let mut pt = PagedTable::new(10);
        pt.next_page(25);
        assert_eq!(pt.page, 2);
        assert_eq!(pt.selected(), Some(0));
    }
}
