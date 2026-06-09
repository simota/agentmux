//! Small text/geometry helpers and terminal-to-ratatui style conversion.

use agentmux_terminal::{CellStyle, TerminalColor};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

pub(crate) fn truncate_to_width(line: &str, width: u16) -> String {
    line.chars().take(usize::from(width)).collect()
}

pub(crate) fn write_line(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    line: &str,
    style: Style,
) {
    for col in 0..width {
        if let Some(cell) = buffer.cell_mut((x + col, y)) {
            cell.set_char(' ');
            cell.set_style(style);
        }
    }
    for (col, ch) in line.chars().take(usize::from(width)).enumerate() {
        let Ok(col) = u16::try_from(col) else {
            break;
        };
        if let Some(cell) = buffer.cell_mut((x + col, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

pub(crate) fn compact_timestamp(value: &str) -> String {
    value
        .strip_suffix("+00:00")
        .unwrap_or(value)
        .replace('T', " ")
}

pub(crate) fn truncate_cell(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut truncated = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

pub(crate) fn centered_rect(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width);
    let height = area.height.min(max_height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

pub fn to_ratatui_style(style: &CellStyle) -> Style {
    let mut out = Style::default();

    if let Some(fg) = style.fg {
        out = out.fg(to_ratatui_color(fg));
    }
    if let Some(bg) = style.bg {
        out = out.bg(to_ratatui_color(bg));
    }

    let mut modifiers = Modifier::empty();
    if style.bold {
        modifiers |= Modifier::BOLD;
    }
    if style.italic {
        modifiers |= Modifier::ITALIC;
    }
    if style.underline {
        modifiers |= Modifier::UNDERLINED;
    }
    if style.reverse {
        modifiers |= Modifier::REVERSED;
    }
    if style.dim {
        modifiers |= Modifier::DIM;
    }

    out.add_modifier(modifiers)
}

pub fn to_ratatui_color(color: TerminalColor) -> Color {
    match color {
        TerminalColor::Indexed(index) => Color::Indexed(index),
        TerminalColor::Rgb { r, g, b } => Color::Rgb(r, g, b),
    }
}
