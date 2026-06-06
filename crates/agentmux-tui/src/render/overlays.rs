//! Centered overlays: session list, provider picker, message bus, activity
//! feed, arena candidates, and keybinding help.

#[cfg(feature = "activity-feed")]
use crate::state::ACTIVITY_FEED_PANE_ID;
use crate::state::TuiSessionState;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

#[cfg(feature = "activity-feed")]
use super::messages::feed_time;
use super::messages::{message_list_lines, render_message_list_panel};
#[cfg(any(feature = "activity-feed", feature = "arena"))]
use super::util::truncate_to_width;
#[cfg(any(feature = "activity-feed", feature = "arena"))]
use super::util::write_line;
use super::util::centered_rect;

const KEYBINDING_HELP_LINES: &[&str] = &[
    "Prefix: Ctrl-g",
    "",
    "Ctrl-g ?      Toggle this help",
    "Ctrl-g d      Detach session",
    "Ctrl-g s      List running sessions",
    "Ctrl-g m      Message bus",
    "Ctrl-g x      Close focused pane",
    "Ctrl-g z      Toggle pane zoom",
    "Ctrl-g [      Copy/scroll focused pane",
    "Ctrl-g arrows Move focus",
    "Ctrl-g %      Split vertical + choose agent",
    "Ctrl-g \"      Split horizontal + choose agent",
    "Msg pane      Enter/Space/d details",
    "Ctrl-g Space  Rotate split direction",
    "Ctrl-g :      Command palette",
];

pub(crate) fn render_session_list(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut lines = vec![
        "Use Up/Down or j/k, Enter to focus, Esc to close".to_string(),
        "".to_string(),
        "  ID NAME ROLE PID".to_string(),
    ];
    for (index, pane) in state
        .panes()
        .filter(|pane| pane.process_id().is_some())
        .enumerate()
    {
        let pid = pane
            .process_id()
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string());
        let role = pane.role().unwrap_or("-");
        let marker = if index == state.session_list_selected_index() {
            ">"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {} {} {} {}",
            pane.agent_id(),
            pane.name(),
            role,
            pid
        ));
    }

    if lines.len() == 3 {
        lines.push("no running sessions".to_string());
    }

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX).min(18);
    let popup = centered_rect(area, 70, height);
    Clear.render(popup, buffer);
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Running Sessions")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(popup, buffer);
}

pub(crate) fn render_provider_picker(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut lines = vec![
        "Use Up/Down or j/k, Enter to start, Esc to close".to_string(),
        "".to_string(),
    ];
    for (index, option) in state.provider_options().iter().enumerate() {
        let marker = if index == state.provider_picker_selected_index() {
            ">"
        } else {
            " "
        };
        lines.push(format!(
            "{marker} {}  {}",
            option.choice.label(),
            option.hint
        ));
    }

    let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX).min(12);
    let popup = centered_rect(area, 72, height);
    Clear.render(popup, buffer);
    let paragraph = Paragraph::new(lines.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("New Coding Agent")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(popup, buffer);
}

pub(crate) fn render_message_bus(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let max_height = area.height.saturating_sub(2).max(8);
    let height = u16::try_from(message_list_lines(state, area.width, true).len() + 2)
        .unwrap_or(u16::MAX)
        .min(max_height);
    let popup = centered_rect(area, area.width.saturating_sub(4).max(40), height);
    render_message_list_panel(popup, state, "Message Bus", false, buffer);
}

#[cfg(feature = "activity-feed")]
pub fn render_activity_feed(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let focused = state.layout().focused() == Some(ACTIVITY_FEED_PANE_ID);
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Activity Feed")
        .border_style(border_style);
    let inner = block.inner(area);
    Clear.render(area, buffer);
    block.render(area, buffer);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let sitrep_rows = state
        .sitrep()
        .len()
        .min(usize::from(inner.height.saturating_sub(1)));
    for (index, entry) in state.sitrep().iter().take(sitrep_rows).enumerate() {
        let Ok(row) = u16::try_from(index) else {
            break;
        };
        let marker = if entry.needs_attention { "!" } else { " " };
        let line = truncate_to_width(
            &format!(
                "{marker} {} {} {}",
                entry.agent_id, entry.name, entry.status
            ),
            inner.width,
        );
        write_line(
            buffer,
            inner.x,
            inner.y + row,
            inner.width,
            &line,
            if entry.needs_attention {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::White)
            },
        );
    }

    let feed_start_row = u16::try_from(sitrep_rows)
        .unwrap_or(u16::MAX)
        .saturating_add(if sitrep_rows > 0 { 1 } else { 0 });
    if feed_start_row >= inner.height {
        return;
    }

    let visible_rows = usize::from(inner.height - feed_start_row);
    let start = state.activity_feed_window_start(visible_rows);
    for (row, (index, entry)) in state
        .feed_entries()
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_rows)
        .enumerate()
    {
        let Ok(row) = u16::try_from(row) else {
            break;
        };
        let marker = if index == state.activity_feed_selected_index() {
            ">"
        } else {
            " "
        };
        let line = truncate_to_width(
            &format!(
                "{marker} [{}] {}  {}  {}",
                feed_time(&entry.ts),
                entry.actor,
                entry.action,
                entry.target
            ),
            inner.width,
        );
        write_line(
            buffer,
            inner.x,
            inner.y + feed_start_row + row,
            inner.width,
            &line,
            if index == state.activity_feed_selected_index() {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                Style::default().fg(Color::White)
            },
        );
    }
}

#[cfg(feature = "arena")]
pub fn render_arena_overlay(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let popup = centered_rect(
        area,
        area.width.saturating_sub(4).max(48),
        area.height.saturating_sub(4).clamp(8, 22),
    );
    Clear.render(popup, buffer);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Arena Candidates")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    block.render(popup, buffer);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    write_line(
        buffer,
        inner.x,
        inner.y,
        inner.width,
        "j/k select | a adopt | Esc/q close",
        Style::default().fg(Color::DarkGray).bg(Color::Black),
    );

    if state.arena_candidates().is_empty() {
        write_line(
            buffer,
            inner.x,
            inner.y.saturating_add(2),
            inner.width,
            "no arena candidates",
            Style::default().fg(Color::White).bg(Color::Black),
        );
        return;
    }

    let count = state.arena_candidates().len();
    let gap = if count > 1 { 1 } else { 0 };
    let panel_width = inner
        .width
        .saturating_sub(u16::try_from(gap * count.saturating_sub(1)).unwrap_or(0))
        / u16::try_from(count).unwrap_or(1).max(1);
    if panel_width == 0 {
        return;
    }

    for (index, candidate) in state.arena_candidates().iter().enumerate() {
        let Ok(index_u16) = u16::try_from(index) else {
            break;
        };
        let x = inner.x + index_u16.saturating_mul(panel_width.saturating_add(1));
        if x >= inner.x.saturating_add(inner.width) {
            break;
        }
        let selected = index == state.arena_selected_index();
        let style = if selected {
            Style::default().fg(Color::Black).bg(Color::White)
        } else {
            Style::default().fg(Color::White).bg(Color::Black)
        };
        let badge_style = match candidate.test_status.as_str() {
            "passed" => Style::default().fg(Color::Green).bg(Color::Black),
            "failed" => Style::default().fg(Color::Red).bg(Color::Black),
            _ => Style::default().fg(Color::Yellow).bg(Color::Black),
        };
        let lines = [
            format!("{} {}", if selected { ">" } else { " " }, candidate.name),
            format!("provider {}", candidate.provider),
            format!("diff     {}", candidate.diff_stat),
            format!("test     {}", candidate.test_status),
            format!("summary  {}", candidate.summary),
            format!("id       {}", candidate.worktree_id),
        ];
        for (row, line) in lines.iter().enumerate() {
            let Ok(row) = u16::try_from(row) else {
                break;
            };
            let y = inner.y.saturating_add(2).saturating_add(row);
            if y >= inner.y.saturating_add(inner.height) {
                break;
            }
            write_line(
                buffer,
                x,
                y,
                panel_width.min(inner.x.saturating_add(inner.width).saturating_sub(x)),
                &truncate_to_width(line, panel_width),
                if row == 3 { badge_style } else { style },
            );
        }
    }
}

pub(crate) fn render_keybinding_help(area: Rect, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let popup = centered_rect(area, 46, 15);
    Clear.render(popup, buffer);
    let paragraph = Paragraph::new(KEYBINDING_HELP_LINES.join("\n"))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Key Bindings")
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(popup, buffer);
}
