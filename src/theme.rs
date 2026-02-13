use ratatui::style::Color;

pub struct Theme {
    pub primary: Color,
    pub primary_dim: Color,
    pub bg_elevated: Color,
    pub border: Color,
    pub border_focus: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub info: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            primary: Color::Rgb(16, 185, 129),
            primary_dim: Color::Rgb(5, 150, 105),
            bg_elevated: Color::Rgb(25, 25, 30),
            border: Color::Rgb(60, 60, 65),
            border_focus: Color::Rgb(16, 185, 129),
            text: Color::Rgb(220, 220, 220),
            text_secondary: Color::Rgb(140, 140, 145),
            text_muted: Color::Rgb(80, 80, 85),
            success: Color::Rgb(34, 197, 94),
            warning: Color::Rgb(234, 179, 8),
            error: Color::Rgb(239, 68, 68),
            info: Color::Rgb(96, 165, 250),
            selection_bg: Color::Rgb(30, 64, 110),
            selection_fg: Color::White,
        }
    }
}

/// Global theme instance
pub fn theme() -> &'static Theme {
    use std::sync::OnceLock;
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(Theme::default)
}
