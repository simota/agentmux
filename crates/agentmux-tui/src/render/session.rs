//! Top-level frame orchestration: composes panes and overlays from client-side
//! TUI state.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

use crate::state::TuiSessionState;

use super::messages::render_message_list_panel;
#[cfg(feature = "activity-feed")]
use super::overlays::render_activity_feed;
#[cfg(feature = "arena")]
use super::overlays::render_arena_overlay;
use super::overlays::{
    render_keybinding_help, render_message_bus, render_provider_picker, render_session_list,
};
use super::pane::{AgentPaneRenderer, PaneChrome};
use super::util::{truncate_to_width, write_line};

/// Renders all daemon-backed panes from client-side TUI state.
#[derive(Clone, Debug, Default)]
pub struct TuiSessionRenderer {
    pane_renderer: AgentPaneRenderer,
}

impl TuiSessionRenderer {
    pub fn render(&self, area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
        for (pane_id, rect) in state.layout().pane_rects(area) {
            if state.is_conversation_list_pane(&pane_id) {
                let focused = state.layout().focused() == Some(pane_id.as_str());
                render_message_list_panel(rect, state, "Conversation List", focused, buffer);
                continue;
            }

            #[cfg(feature = "activity-feed")]
            if state.is_activity_feed_pane(&pane_id) {
                render_activity_feed(rect, state, buffer);
                continue;
            }

            let Some(pane) = state.pane(&pane_id) else {
                continue;
            };
            let focused = state.layout().focused() == Some(pane.agent_id());
            let chrome = PaneChrome::new(pane_chrome_title(
                &pane.chrome_title(),
                focused,
                pane.has_unseen_output(),
            ))
            .focused(focused);
            let selection = state
                .copy_selection()
                .filter(|selection| selection.agent_id == pane.agent_id());
            self.pane_renderer.render_scrolled_with_selection(
                rect,
                pane.grid(),
                pane.scroll_offset(),
                &chrome,
                selection,
                buffer,
            );
        }

        if state.keybinding_help_visible() {
            render_keybinding_help(area, buffer);
        }

        if state.session_list_visible() {
            render_session_list(area, state, buffer);
        }

        if state.provider_picker_visible() {
            render_provider_picker(area, state, buffer);
        }

        if state.message_bus_visible() {
            render_message_bus(area, state, buffer);
        }

        #[cfg(feature = "arena")]
        if state.arena_overlay_visible() {
            render_arena_overlay(area, state, buffer);
        }

        render_status_bar(area, state, buffer);

        if let Some(notice) = state.runtime_notice() {
            render_runtime_notice(area, notice, buffer);
        }
    }
}

/// Glyph prepended to the focused pane's title so focus is legible without
/// relying on border color alone (works in low-contrast themes).
const FOCUS_MARKER: &str = "▶ ";
/// Glyph appended to an unfocused pane that has received output the user has
/// not yet seen.
const UNSEEN_MARKER: &str = " ●";

/// Compose a pane's title with focus and unseen-content markers.
fn pane_chrome_title(base: &str, focused: bool, has_unseen: bool) -> String {
    let mut title = String::new();
    if focused {
        title.push_str(FOCUS_MARKER);
    }
    title.push_str(base);
    if !focused && has_unseen {
        title.push_str(UNSEEN_MARKER);
    }
    title
}

/// Render the bottom status bar: prefix-mode indicator and focused-pane label.
fn render_status_bar(area: Rect, state: &TuiSessionState, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut segments: Vec<String> = Vec::new();
    if state.prefix_active() {
        segments.push("PREFIX (Ctrl-g)".to_string());
    }
    if let Some(focus) = state.focus_position_label() {
        segments.push(focus);
    }
    if segments.is_empty() {
        return;
    }

    let line = segments.join("  |  ");
    let style = if state.prefix_active() {
        Style::default().fg(Color::Black).bg(Color::Yellow)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    };
    write_line(
        buffer,
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        &truncate_to_width(&line, area.width),
        style,
    );
}

fn render_runtime_notice(area: Rect, notice: &str, buffer: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    write_line(
        buffer,
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        &truncate_to_width(notice, area.width),
        Style::default().fg(Color::Yellow).bg(Color::Black),
    );
}
