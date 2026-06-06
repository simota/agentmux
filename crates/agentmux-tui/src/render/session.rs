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
            let chrome = PaneChrome::new(pane.chrome_title())
                .focused(state.layout().focused() == Some(pane.agent_id()));
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

        if let Some(notice) = state.runtime_notice() {
            render_runtime_notice(area, notice, buffer);
        }
    }
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
