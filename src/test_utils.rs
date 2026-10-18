use crate::repository::Repository;

pub fn test_repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::TempDir::new().unwrap();
    let conn = crate::db::init_db(&dir.path().join("test.db")).unwrap();
    (dir, Repository::new(conn))
}

/// Draw one frame with `draw` on a `width`x`height` test terminal and return
/// the screen as text, one line per row.
pub fn render_to_text(width: u16, height: u16, draw: impl FnOnce(&mut ratatui::Frame)) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(draw).unwrap();
    let buf = terminal.backend().buffer();
    let area = buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
