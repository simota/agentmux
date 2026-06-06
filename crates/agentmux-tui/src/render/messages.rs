//! Message-bus / conversation-list panel rendering and message line formatting.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::state::{MessageListItem, TuiSessionState};

use super::util::{compact_timestamp, truncate_cell};

pub(crate) fn render_message_list_panel(
    area: Rect,
    state: &TuiSessionState,
    title: &'static str,
    focused: bool,
    buffer: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let overlay = title == "Message Bus";
    let lines = message_list_lines(state, area.width, overlay);
    let visible_lines = area.height.saturating_sub(2) as usize;
    let text = lines
        .into_iter()
        .take(visible_lines)
        .collect::<Vec<_>>()
        .join("\n");

    Clear.render(area, buffer);
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Cyan)
    };
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(message_list_title(title, state.message_details_visible()))
                .border_style(border_style),
        )
        .alignment(Alignment::Left)
        .style(Style::default().fg(Color::White).bg(Color::Black));
    paragraph.render(area, buffer);
}

#[cfg(feature = "activity-feed")]
pub(crate) fn feed_time(ts: &str) -> String {
    if ts.len() >= 19 && ts.as_bytes().get(10) == Some(&b'T') {
        return ts[11..19].to_string();
    }
    ts.to_string()
}

pub(crate) fn message_list_title(title: &'static str, details_visible: bool) -> String {
    let mode = if details_visible {
        "details"
    } else {
        "compact"
    };
    format!("{title} [{mode}]")
}

pub(crate) fn message_list_lines(
    state: &TuiSessionState,
    area_width: u16,
    include_overlay_hint: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    let next_mode = if state.message_details_visible() {
        "compact"
    } else {
        "details"
    };
    if include_overlay_hint {
        lines.push(format!("Enter/Space/d {next_mode} | Esc/q close"));
    } else {
        lines.push(format!("Enter/Space/d {next_mode} | Ctrl-g x close"));
    }
    lines.push("".to_string());

    let content_width = usize::from(area_width.saturating_sub(4)).clamp(24, 120);

    for message in state.messages().iter() {
        if state.message_details_visible() {
            lines.extend(message_detail_lines(message, content_width));
        } else {
            lines.extend(message_compact_lines(message, content_width));
        }
    }

    if state.messages().is_empty() {
        lines.push("no messages".to_string());
    }

    lines
}

fn message_compact_lines(message: &MessageListItem, content_width: usize) -> Vec<String> {
    let meta = format!(
        "{} / {} / {} / {}",
        message.delivery_status,
        message.kind,
        message.message_id,
        compact_timestamp(&message.created_at)
    );
    let mut route = format!("{} -> {}", message.from, message.to);
    if let Some(thread_id) = &message.thread_id {
        route.push_str(&format!(" [{}]", short_thread_label(thread_id)));
    }
    vec![
        truncate_cell(&meta, content_width),
        truncate_cell(&route, content_width),
        truncate_cell(&message.body, content_width),
        "".to_string(),
    ]
}

/// Compact `thread_01ABCDEF…` to `thread:…CDEF` so the conversation list can
/// show which meeting a message belongs to without consuming a whole column.
fn short_thread_label(thread_id: &str) -> String {
    let tail: String = thread_id
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("thread:…{tail}")
}

fn message_detail_lines(message: &MessageListItem, content_width: usize) -> Vec<String> {
    let mut lines = vec![
        truncate_cell(
            &format!("{} / {}", message.delivery_status, message.kind),
            content_width,
        ),
        truncate_cell(&format!("id: {}", message.message_id), content_width),
        truncate_cell(
            &format!("created: {}", compact_timestamp(&message.created_at)),
            content_width,
        ),
        truncate_cell(&format!("from: {}", message.from), content_width),
        truncate_cell(&format!("to: {}", message.to), content_width),
    ];
    if let Some(thread_id) = &message.thread_id {
        lines.push(truncate_cell(
            &format!("thread: {thread_id}"),
            content_width,
        ));
    }
    lines.push(truncate_cell(
        &format!("body: {}", message.body),
        content_width,
    ));
    lines.push("".to_string());
    lines
}
