use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Available theme presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    /// Rich RGB colors for dark terminal backgrounds (default).
    #[default]
    Dark,
    /// High-contrast colors for light terminal backgrounds.
    Light,
    /// Uses only the terminal's 16 ANSI colors — adapts to any color scheme.
    Terminal,
}

impl ThemeName {
    /// Cycle to the next theme preset.
    pub const fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Terminal,
            Self::Terminal => Self::Dark,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Terminal => "terminal",
        }
    }
}

impl std::fmt::Display for ThemeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    pub badge_bg: Color,
    pub risk_critical: Color,
    pub risk_high: Color,
    pub risk_medium: Color,
    pub risk_low: Color,
}

impl Theme {
    const fn dark() -> Self {
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
            badge_bg: Color::Rgb(50, 50, 55),
            risk_critical: Color::Rgb(239, 68, 68),
            risk_high: Color::Rgb(251, 146, 60),
            risk_medium: Color::Rgb(234, 179, 8),
            risk_low: Color::Rgb(120, 120, 130),
        }
    }

    const fn light() -> Self {
        Self {
            primary: Color::Rgb(5, 122, 85),
            primary_dim: Color::Rgb(4, 100, 70),
            bg_elevated: Color::Rgb(245, 245, 245),
            border: Color::Rgb(180, 180, 185),
            border_focus: Color::Rgb(5, 122, 85),
            text: Color::Rgb(30, 30, 30),
            text_secondary: Color::Rgb(90, 90, 95),
            text_muted: Color::Rgb(150, 150, 155),
            success: Color::Rgb(22, 163, 74),
            warning: Color::Rgb(180, 130, 0),
            error: Color::Rgb(200, 40, 40),
            info: Color::Rgb(37, 99, 235),
            selection_bg: Color::Rgb(200, 225, 255),
            selection_fg: Color::Rgb(20, 20, 20),
            badge_bg: Color::Rgb(220, 220, 225),
            risk_critical: Color::Rgb(200, 40, 40),
            risk_high: Color::Rgb(210, 110, 20),
            risk_medium: Color::Rgb(160, 120, 0),
            risk_low: Color::Rgb(120, 120, 130),
        }
    }

    const fn terminal() -> Self {
        Self {
            primary: Color::Green,
            primary_dim: Color::DarkGray,
            bg_elevated: Color::Reset,
            border: Color::DarkGray,
            border_focus: Color::Green,
            text: Color::Reset,
            text_secondary: Color::Gray,
            text_muted: Color::DarkGray,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            info: Color::Blue,
            selection_bg: Color::Blue,
            selection_fg: Color::White,
            badge_bg: Color::DarkGray,
            risk_critical: Color::Red,
            risk_high: Color::LightRed,
            risk_medium: Color::Yellow,
            risk_low: Color::DarkGray,
        }
    }

    const fn from_name(name: ThemeName) -> Self {
        match name {
            ThemeName::Dark => Self::dark(),
            ThemeName::Light => Self::light(),
            ThemeName::Terminal => Self::terminal(),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

static GLOBAL_THEME: OnceLock<Theme> = OnceLock::new();

/// Initialize the global theme. Call once at startup before any TUI rendering.
/// If called multiple times, only the first call takes effect.
pub fn init_theme(name: ThemeName) {
    let _ = GLOBAL_THEME.set(Theme::from_name(name));
}

/// Get the global theme. Falls back to dark theme if `init_theme` was not called.
pub fn theme() -> &'static Theme {
    GLOBAL_THEME.get_or_init(Theme::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_name_cycle() {
        assert_eq!(ThemeName::Dark.next(), ThemeName::Light);
        assert_eq!(ThemeName::Light.next(), ThemeName::Terminal);
        assert_eq!(ThemeName::Terminal.next(), ThemeName::Dark);
    }

    #[test]
    fn test_theme_name_display() {
        assert_eq!(ThemeName::Dark.as_str(), "dark");
        assert_eq!(ThemeName::Light.as_str(), "light");
        assert_eq!(ThemeName::Terminal.as_str(), "terminal");
    }

    #[test]
    fn test_theme_name_default() {
        assert_eq!(ThemeName::default(), ThemeName::Dark);
    }

    #[test]
    fn test_theme_name_serde_roundtrip() {
        let name = ThemeName::Light;
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"light\"");
        let parsed: ThemeName = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ThemeName::Light);
    }

    #[test]
    fn test_dark_theme_colors() {
        let t = Theme::dark();
        assert_ne!(format!("{:?}", t.primary), format!("{:?}", t.error));
    }

    #[test]
    fn test_light_theme_colors() {
        let t = Theme::light();
        assert_ne!(format!("{:?}", t.primary), format!("{:?}", t.error));
    }

    #[test]
    fn test_terminal_theme_uses_ansi() {
        let t = Theme::terminal();
        assert_eq!(t.primary, Color::Green);
        assert_eq!(t.error, Color::Red);
        assert_eq!(t.warning, Color::Yellow);
    }

    #[test]
    fn test_from_name() {
        let t = Theme::from_name(ThemeName::Terminal);
        assert_eq!(t.primary, Color::Green);
    }
}
