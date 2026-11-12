use ratatui::style::{Color, Style};

pub const BG: Color = Color::Rgb(13, 15, 18);
pub const PANEL: Color = Color::Rgb(18, 21, 25);
pub const PANEL_ALT: Color = Color::Rgb(23, 27, 32);
pub const FG: Color = Color::Rgb(198, 203, 210);
pub const DIM: Color = Color::Rgb(112, 119, 129);
pub const FAINT: Color = Color::Rgb(64, 71, 82);
pub const ACCENT: Color = Color::Rgb(107, 163, 255);
pub const GREEN: Color = Color::Rgb(137, 192, 140);
pub const AMBER: Color = Color::Rgb(218, 173, 105);
pub const RED: Color = Color::Rgb(220, 118, 120);
pub const SELECT: Color = Color::Rgb(36, 45, 57);
pub const HOVER: Color = Color::Rgb(28, 34, 41);
pub const SELECTION: Color = Color::Rgb(47, 64, 86);
pub const CURSOR_LINE: Color = Color::Rgb(22, 26, 32);
pub const BORDER: Color = Color::Rgb(39, 45, 54);
pub const READ_BG: Color = Color::Rgb(21, 34, 48);
pub const WRITE_BG: Color = Color::Rgb(40, 35, 23);

pub fn base() -> Style {
    Style::default().fg(FG).bg(BG)
}
