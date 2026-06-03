//! Pane layout management.

use ratatui::layout::{Constraint, Direction, Layout};

/// A rectangular region on the terminal, in character cells.
///
/// Re-exported from `ratatui` for convenience.
pub use ratatui::layout::Rect;

/// Stable pane identifier used by the client-side layout state.
pub type PaneId = String;

/// Direction used when splitting the visible terminal area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl SplitDirection {
    fn ratatui(self) -> Direction {
        match self {
            Self::Horizontal => Direction::Vertical,
            Self::Vertical => Direction::Horizontal,
        }
    }
}

/// Minimal serializable state needed to restore a detached client view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneSnapshot {
    pub panes: Vec<PaneId>,
    pub split_direction: SplitDirection,
    pub focused: Option<PaneId>,
    pub zoomed: bool,
}

/// Client-side pane layout for tmux-like split/focus/zoom behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneLayout {
    panes: Vec<PaneId>,
    split_direction: SplitDirection,
    focused: Option<PaneId>,
    zoomed: bool,
}

impl PaneLayout {
    pub fn new(split_direction: SplitDirection) -> Self {
        Self {
            panes: Vec::new(),
            split_direction,
            focused: None,
            zoomed: false,
        }
    }

    pub fn with_panes<I, S>(split_direction: SplitDirection, panes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<PaneId>,
    {
        let panes = panes.into_iter().map(Into::into).collect::<Vec<_>>();
        let focused = panes.first().cloned();
        Self {
            panes,
            split_direction,
            focused,
            zoomed: false,
        }
    }

    pub fn panes(&self) -> &[PaneId] {
        &self.panes
    }

    pub fn split_direction(&self) -> SplitDirection {
        self.split_direction
    }

    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    pub fn add_pane(&mut self, pane_id: impl Into<PaneId>) {
        let pane_id = pane_id.into();
        if self.panes.iter().any(|existing| existing == &pane_id) {
            return;
        }

        if self.focused.is_none() {
            self.focused = Some(pane_id.clone());
        }
        self.panes.push(pane_id);
    }

    pub fn focus(&mut self, pane_id: &str) -> bool {
        if self.panes.iter().any(|existing| existing == pane_id) {
            self.focused = Some(pane_id.to_owned());
            true
        } else {
            false
        }
    }

    pub fn focus_next(&mut self) {
        if self.panes.is_empty() {
            self.focused = None;
            return;
        }

        let current = self
            .focused
            .as_ref()
            .and_then(|focused| self.panes.iter().position(|pane| pane == focused))
            .unwrap_or(0);
        let next = (current + 1) % self.panes.len();
        self.focused = Some(self.panes[next].clone());
    }

    pub fn focus_previous(&mut self) {
        if self.panes.is_empty() {
            self.focused = None;
            return;
        }

        let current = self
            .focused
            .as_ref()
            .and_then(|focused| self.panes.iter().position(|pane| pane == focused))
            .unwrap_or(0);
        let previous = current
            .checked_sub(1)
            .unwrap_or_else(|| self.panes.len().saturating_sub(1));
        self.focused = Some(self.panes[previous].clone());
    }

    pub fn remove_pane(&mut self, pane_id: &str) -> bool {
        let Some(index) = self.panes.iter().position(|existing| existing == pane_id) else {
            return false;
        };
        self.panes.remove(index);

        if self.focused.as_deref() == Some(pane_id) {
            self.focused = if self.panes.is_empty() {
                None
            } else {
                Some(self.panes[index.min(self.panes.len() - 1)].clone())
            };
        }

        if self.panes.is_empty() {
            self.zoomed = false;
        }
        true
    }

    pub fn set_split_direction(&mut self, split_direction: SplitDirection) {
        self.split_direction = split_direction;
    }

    pub fn toggle_split_direction(&mut self) {
        self.split_direction = match self.split_direction {
            SplitDirection::Horizontal => SplitDirection::Vertical,
            SplitDirection::Vertical => SplitDirection::Horizontal,
        };
    }

    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
    }

    pub fn set_zoomed(&mut self, zoomed: bool) {
        self.zoomed = zoomed;
    }

    pub fn pane_rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        if area.width == 0 || area.height == 0 || self.panes.is_empty() {
            return Vec::new();
        }

        if self.zoomed {
            let focused = self.focused.as_ref().unwrap_or(&self.panes[0]);
            return vec![(focused.clone(), area)];
        }

        let constraints = even_constraints(self.panes.len());
        let chunks = Layout::default()
            .direction(self.split_direction.ratatui())
            .constraints(constraints)
            .split(area);

        self.panes
            .iter()
            .zip(chunks.iter())
            .map(|(pane_id, rect)| (pane_id.clone(), *rect))
            .collect()
    }

    pub fn pane_inner_size(rect: Rect) -> (u16, u16) {
        (rect.height.saturating_sub(2), rect.width.saturating_sub(2))
    }

    pub fn snapshot(&self) -> PaneSnapshot {
        PaneSnapshot {
            panes: self.panes.clone(),
            split_direction: self.split_direction,
            focused: self.focused.clone(),
            zoomed: self.zoomed,
        }
    }

    pub fn restore(snapshot: PaneSnapshot) -> Self {
        let focused = snapshot.focused.and_then(|focused| {
            snapshot
                .panes
                .iter()
                .any(|pane| pane == &focused)
                .then_some(focused)
        });
        let focused = focused.or_else(|| snapshot.panes.first().cloned());

        Self {
            panes: snapshot.panes,
            split_direction: snapshot.split_direction,
            focused,
            zoomed: snapshot.zoomed,
        }
    }
}

fn even_constraints(len: usize) -> Vec<Constraint> {
    if len == 0 {
        return Vec::new();
    }

    let base = 100 / len as u16;
    let remainder = 100 % len as u16;
    (0..len)
        .map(|index| Constraint::Percentage(base + u16::from(index < usize::from(remainder))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_split_allocates_two_side_by_side_panes() {
        let layout = PaneLayout::with_panes(SplitDirection::Vertical, ["left", "right"]);

        let rects = layout.pane_rects(Rect::new(0, 0, 100, 24));

        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], ("left".to_owned(), Rect::new(0, 0, 50, 24)));
        assert_eq!(rects[1], ("right".to_owned(), Rect::new(50, 0, 50, 24)));
    }

    #[test]
    fn horizontal_split_allocates_stacked_panes() {
        let layout = PaneLayout::with_panes(SplitDirection::Horizontal, ["top", "bottom"]);

        let rects = layout.pane_rects(Rect::new(0, 0, 80, 20));

        assert_eq!(rects[0], ("top".to_owned(), Rect::new(0, 0, 80, 10)));
        assert_eq!(rects[1], ("bottom".to_owned(), Rect::new(0, 10, 80, 10)));
    }

    #[test]
    fn zoom_renders_only_focused_pane_to_full_area() {
        let mut layout = PaneLayout::with_panes(SplitDirection::Vertical, ["one", "two"]);
        assert!(layout.focus("two"));
        layout.set_zoomed(true);

        let rects = layout.pane_rects(Rect::new(2, 3, 70, 19));

        assert_eq!(rects, vec![("two".to_owned(), Rect::new(2, 3, 70, 19))]);
    }

    #[test]
    fn snapshot_restore_preserves_focus_zoom_and_split() {
        let mut layout = PaneLayout::with_panes(SplitDirection::Horizontal, ["a", "b", "c"]);
        assert!(layout.focus("c"));
        layout.set_zoomed(true);

        let restored = PaneLayout::restore(layout.snapshot());

        assert_eq!(
            restored.panes(),
            &["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
        assert_eq!(restored.focused(), Some("c"));
        assert_eq!(restored.split_direction(), SplitDirection::Horizontal);
        assert!(restored.is_zoomed());
    }

    #[test]
    fn inner_size_accounts_for_border_without_underflow() {
        assert_eq!(PaneLayout::pane_inner_size(Rect::new(0, 0, 10, 5)), (3, 8));
        assert_eq!(PaneLayout::pane_inner_size(Rect::new(0, 0, 1, 1)), (0, 0));
    }

    #[test]
    fn focus_previous_wraps_to_last_pane() {
        let mut layout = PaneLayout::with_panes(SplitDirection::Vertical, ["a", "b", "c"]);

        layout.focus_previous();

        assert_eq!(layout.focused(), Some("c"));
    }

    #[test]
    fn remove_focused_pane_moves_focus_to_neighbor() {
        let mut layout = PaneLayout::with_panes(SplitDirection::Vertical, ["a", "b", "c"]);
        assert!(layout.focus("b"));

        assert!(layout.remove_pane("b"));

        assert_eq!(layout.panes(), &["a".to_owned(), "c".to_owned()]);
        assert_eq!(layout.focused(), Some("c"));
    }

    #[test]
    fn toggling_split_direction_switches_orientation() {
        let mut layout = PaneLayout::with_panes(SplitDirection::Vertical, ["a", "b"]);

        layout.toggle_split_direction();

        assert_eq!(layout.split_direction(), SplitDirection::Horizontal);
    }
}
