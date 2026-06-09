//! Pane layout management.
//!
//! The layout is a recursive tree of [`LayoutNode`]s. Flat configurations — every
//! leaf directly under the root [`LayoutNode::Split`] — preserve the historical
//! `Vec<PaneId>` behavior bit-for-bit: leaf order, focus traversal, rect geometry,
//! and dynamic `add_pane` insertion all match the pre-tree implementation. Nesting
//! and per-node size ratios only take effect through CLI-parsed startup layouts and
//! the `pane_rects` renderer; runtime mutations keep the flat (root-level) behavior.

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

/// A node in the pane layout tree.
///
/// A `Leaf` carries a single pane. A `Split` arranges its children along
/// `direction` (left-right for [`SplitDirection::Vertical`], top-bottom for
/// [`SplitDirection::Horizontal`]).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutNode {
    Leaf { pane_id: PaneId },
    Split {
        direction: SplitDirection,
        children: Vec<LayoutChild>,
    },
}

/// A child slot inside a [`LayoutNode::Split`].
///
/// `size` is the unitless ratio this child occupies within its parent split.
/// `None` means "share the remaining space evenly with the other un-sized
/// children".
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayoutChild {
    pub node: LayoutNode,
    pub size: Option<u16>,
}

impl LayoutChild {
    pub fn new(node: LayoutNode) -> Self {
        Self { node, size: None }
    }

    pub fn sized(node: LayoutNode, size: Option<u16>) -> Self {
        Self { node, size }
    }
}

impl LayoutNode {
    /// Convenience constructor for a leaf.
    pub fn leaf(pane_id: impl Into<PaneId>) -> Self {
        LayoutNode::Leaf {
            pane_id: pane_id.into(),
        }
    }

    /// An empty layout: a split with no children. `panes()` reports no panes.
    fn empty(direction: SplitDirection) -> Self {
        LayoutNode::Split {
            direction,
            children: Vec::new(),
        }
    }

    /// Collect leaf pane ids in depth-first (in-order) traversal.
    fn collect_leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Leaf { pane_id } => out.push(pane_id.clone()),
            LayoutNode::Split { children, .. } => {
                for child in children {
                    child.node.collect_leaves(out);
                }
            }
        }
    }

    /// Walk the tree and emit `(PaneId, Rect)` for each leaf, recursively
    /// splitting `area` at every `Split` node. Order matches DFS leaf order.
    fn layout_rects(&self, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            LayoutNode::Leaf { pane_id } => out.push((pane_id.clone(), area)),
            LayoutNode::Split {
                direction,
                children,
            } => {
                if children.is_empty() {
                    return;
                }
                let constraints = child_constraints(children);
                let chunks = Layout::default()
                    .direction(direction.ratatui())
                    .constraints(constraints)
                    .split(area);
                for (child, rect) in children.iter().zip(chunks.iter()) {
                    child.node.layout_rects(*rect, out);
                }
            }
        }
    }

    /// Append a leaf at the end of the root level. Used by dynamic `add_pane`.
    fn push_root_leaf(&mut self, pane_id: PaneId) {
        match self {
            LayoutNode::Split { children, .. } => {
                children.push(LayoutChild::new(LayoutNode::leaf(pane_id)));
            }
            // A bare leaf root is promoted to a split holding the existing leaf
            // plus the new one, preserving in-order traversal.
            LayoutNode::Leaf { .. } => {
                let existing = std::mem::replace(self, LayoutNode::empty(SplitDirection::Vertical));
                if let LayoutNode::Split { children, .. } = self {
                    children.push(LayoutChild::new(existing));
                    children.push(LayoutChild::new(LayoutNode::leaf(pane_id)));
                }
            }
        }
    }

    /// Remove the leaf with `pane_id` anywhere in the tree, pruning now-empty
    /// splits. Returns `true` if a leaf was removed.
    fn remove_leaf(&mut self, pane_id: &str) -> bool {
        match self {
            LayoutNode::Leaf { .. } => false,
            LayoutNode::Split { children, .. } => {
                if let Some(index) = children.iter().position(|child| {
                    matches!(&child.node, LayoutNode::Leaf { pane_id: id } if id == pane_id)
                }) {
                    children.remove(index);
                    return true;
                }
                for child in children.iter_mut() {
                    if child.node.remove_leaf(pane_id) {
                        return true;
                    }
                }
                false
            }
        }
    }
}

/// Minimal serializable state needed to restore a detached client view.
///
/// This rides only in-process (it never travels over IPC), so it carries the full
/// layout tree rather than a flattened list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneSnapshot {
    pub root: LayoutNode,
    pub focused: Option<PaneId>,
    pub zoomed: bool,
}

/// Client-side pane layout for tmux-like split/focus/zoom behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneLayout {
    root: LayoutNode,
    /// Cached DFS leaf order, kept in sync with `root` on every mutation so
    /// `panes()` can return a borrowed slice cheaply.
    leaf_order: Vec<PaneId>,
    focused: Option<PaneId>,
    zoomed: bool,
}

impl PaneLayout {
    pub fn new(split_direction: SplitDirection) -> Self {
        Self::from_root(LayoutNode::empty(split_direction))
    }

    pub fn with_panes<I, S>(split_direction: SplitDirection, panes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<PaneId>,
    {
        let children = panes
            .into_iter()
            .map(|pane| LayoutChild::new(LayoutNode::leaf(pane.into())))
            .collect::<Vec<_>>();
        Self::from_root(LayoutNode::Split {
            direction: split_direction,
            children,
        })
    }

    /// Build a layout from a normalized tree. Focus defaults to the first leaf.
    pub fn from_root(root: LayoutNode) -> Self {
        let root = normalize(root);
        let mut leaf_order = Vec::new();
        root.collect_leaves(&mut leaf_order);
        let focused = leaf_order.first().cloned();
        Self {
            root,
            leaf_order,
            focused,
            zoomed: false,
        }
    }

    fn recompute_leaves(&mut self) {
        self.leaf_order.clear();
        self.root.collect_leaves(&mut self.leaf_order);
    }

    pub fn panes(&self) -> &[PaneId] {
        &self.leaf_order
    }

    pub fn root(&self) -> &LayoutNode {
        &self.root
    }

    /// The direction of the root split. For a flat layout this is the historical
    /// `split_direction`. A bare-leaf root reports `Vertical` (the prior default).
    pub fn split_direction(&self) -> SplitDirection {
        match &self.root {
            LayoutNode::Split { direction, .. } => *direction,
            LayoutNode::Leaf { .. } => SplitDirection::Vertical,
        }
    }

    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    pub fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    pub fn add_pane(&mut self, pane_id: impl Into<PaneId>) {
        let pane_id = pane_id.into();
        if self.leaf_order.iter().any(|existing| existing == &pane_id) {
            return;
        }

        if self.focused.is_none() {
            self.focused = Some(pane_id.clone());
        }
        self.root.push_root_leaf(pane_id);
        self.recompute_leaves();
    }

    pub fn focus(&mut self, pane_id: &str) -> bool {
        if self.leaf_order.iter().any(|existing| existing == pane_id) {
            self.focused = Some(pane_id.to_owned());
            true
        } else {
            false
        }
    }

    pub fn focus_next(&mut self) {
        if self.leaf_order.is_empty() {
            self.focused = None;
            return;
        }

        let current = self
            .focused
            .as_ref()
            .and_then(|focused| self.leaf_order.iter().position(|pane| pane == focused))
            .unwrap_or(0);
        let next = (current + 1) % self.leaf_order.len();
        self.focused = Some(self.leaf_order[next].clone());
    }

    pub fn focus_previous(&mut self) {
        if self.leaf_order.is_empty() {
            self.focused = None;
            return;
        }

        let current = self
            .focused
            .as_ref()
            .and_then(|focused| self.leaf_order.iter().position(|pane| pane == focused))
            .unwrap_or(0);
        let previous = current
            .checked_sub(1)
            .unwrap_or_else(|| self.leaf_order.len().saturating_sub(1));
        self.focused = Some(self.leaf_order[previous].clone());
    }

    pub fn remove_pane(&mut self, pane_id: &str) -> bool {
        let Some(index) = self.leaf_order.iter().position(|existing| existing == pane_id) else {
            return false;
        };
        self.root.remove_leaf(pane_id);
        self.recompute_leaves();

        if self.focused.as_deref() == Some(pane_id) {
            self.focused = if self.leaf_order.is_empty() {
                None
            } else {
                Some(self.leaf_order[index.min(self.leaf_order.len() - 1)].clone())
            };
        }

        if self.leaf_order.is_empty() {
            self.zoomed = false;
        }
        true
    }

    pub fn set_split_direction(&mut self, split_direction: SplitDirection) {
        if let LayoutNode::Split { direction, .. } = &mut self.root {
            *direction = split_direction;
        }
    }

    pub fn toggle_split_direction(&mut self) {
        if let LayoutNode::Split { direction, .. } = &mut self.root {
            *direction = match direction {
                SplitDirection::Horizontal => SplitDirection::Vertical,
                SplitDirection::Vertical => SplitDirection::Horizontal,
            };
        }
    }

    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
    }

    pub fn set_zoomed(&mut self, zoomed: bool) {
        self.zoomed = zoomed;
    }

    pub fn pane_rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        if area.width == 0 || area.height == 0 || self.leaf_order.is_empty() {
            return Vec::new();
        }

        if self.zoomed {
            let focused = self.focused.as_ref().unwrap_or(&self.leaf_order[0]);
            return vec![(focused.clone(), area)];
        }

        let mut rects = Vec::with_capacity(self.leaf_order.len());
        self.root.layout_rects(area, &mut rects);
        rects
    }

    /// Build a transient layout that appends `pane_id` as a new leaf at the end
    /// of the root level — exactly where dynamic `add_pane` would place it. Used
    /// to compute the rect a not-yet-spawned pane would occupy.
    pub fn with_pending_pane(&self, pane_id: impl Into<PaneId>) -> PaneLayout {
        let pane_id = pane_id.into();
        let mut root = self.root.clone();
        root.push_root_leaf(pane_id.clone());
        let mut layout = PaneLayout::from_root(root);
        layout.focused = Some(pane_id);
        layout
    }

    pub fn pane_inner_size(rect: Rect) -> (u16, u16) {
        (rect.height.saturating_sub(2), rect.width.saturating_sub(2))
    }

    pub fn snapshot(&self) -> PaneSnapshot {
        PaneSnapshot {
            root: self.root.clone(),
            focused: self.focused.clone(),
            zoomed: self.zoomed,
        }
    }

    pub fn restore(snapshot: PaneSnapshot) -> Self {
        let root = normalize(snapshot.root);
        let mut leaf_order = Vec::new();
        root.collect_leaves(&mut leaf_order);

        let focused = snapshot
            .focused
            .filter(|focused| leaf_order.iter().any(|pane| pane == focused))
            .or_else(|| leaf_order.first().cloned());

        Self {
            root,
            leaf_order,
            focused,
            zoomed: snapshot.zoomed,
        }
    }
}

/// Normalize a layout tree: collapse single-child splits into their child so a
/// `Split` always holds a real split (`(agy)` becomes `Leaf(agy)`).
fn normalize(node: LayoutNode) -> LayoutNode {
    match node {
        LayoutNode::Leaf { .. } => node,
        LayoutNode::Split {
            direction,
            children,
        } => {
            let mut normalized = children
                .into_iter()
                .map(|child| LayoutChild::sized(normalize(child.node), child.size))
                .collect::<Vec<_>>();
            if normalized.len() == 1 {
                // A single-child split is just that child; drop the wrapper but
                // keep the inner node (its own size context is the parent's).
                return normalized.remove(0).node;
            }
            LayoutNode::Split {
                direction,
                children: normalized,
            }
        }
    }
}

/// Compute the percentage constraints for a split's children.
///
/// - All `None`: even split with the remainder handed to the leading children
///   (identical to the historical `even_constraints`).
/// - All `Some`: ratios normalized to sum 100, remainder to leading children.
/// - Mixed: sized children take their share first, the remaining percentage is
///   distributed evenly among the un-sized children.
fn child_constraints(children: &[LayoutChild]) -> Vec<Constraint> {
    let len = children.len();
    if len == 0 {
        return Vec::new();
    }

    let sized_total: u32 = children
        .iter()
        .filter_map(|child| child.size)
        .map(u32::from)
        .sum();
    let sized_count = children.iter().filter(|child| child.size.is_some()).count();
    let unsized_count = len - sized_count;

    if sized_count == 0 {
        return even_constraints(len);
    }

    // Resolve the percentage each sized child occupies.
    // When the ratios already fit in 100 (and there are un-sized children to
    // absorb the rest) we use the literal values; otherwise normalize by the
    // total so the split never exceeds 100%.
    let normalize_sized = sized_total > 100 || (unsized_count == 0 && sized_total != 100);
    let mut percentages = vec![0u16; len];
    let mut assigned: u32 = 0;
    for (index, child) in children.iter().enumerate() {
        if let Some(size) = child.size {
            let pct = if normalize_sized {
                ((u32::from(size) * 100) / sized_total) as u16
            } else {
                size
            };
            percentages[index] = pct;
            assigned += u32::from(pct);
        }
    }

    if unsized_count == 0 {
        // Distribute the rounding remainder to the leading sized children.
        let mut remainder = 100u32.saturating_sub(assigned);
        for child_index in (0..len).filter(|&i| children[i].size.is_some()) {
            if remainder == 0 {
                break;
            }
            percentages[child_index] += 1;
            remainder -= 1;
        }
    } else {
        // Hand the leftover percentage to the un-sized children, evenly, with the
        // remainder going to the leading ones. Never underflow: clamp at zero.
        let leftover = 100u32.saturating_sub(assigned);
        let base = (leftover / unsized_count as u32) as u16;
        let extra = (leftover % unsized_count as u32) as u16;
        let mut seen_unsized = 0u16;
        for (index, child) in children.iter().enumerate() {
            if child.size.is_none() {
                percentages[index] = base + u16::from(seen_unsized < extra);
                seen_unsized += 1;
            }
        }
    }

    percentages
        .into_iter()
        .map(Constraint::Percentage)
        .collect()
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
    fn snapshot_restore_round_trips_a_nested_tree() {
        // (agy ― codex) | messages
        let root = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            children: vec![
                LayoutChild::new(LayoutNode::Split {
                    direction: SplitDirection::Horizontal,
                    children: vec![
                        LayoutChild::new(LayoutNode::leaf("agy")),
                        LayoutChild::new(LayoutNode::leaf("codex")),
                    ],
                }),
                LayoutChild::new(LayoutNode::leaf("messages")),
            ],
        };
        let layout = PaneLayout::from_root(root.clone());

        let restored = PaneLayout::restore(layout.snapshot());

        assert_eq!(restored.root(), &root);
        assert_eq!(
            restored.panes(),
            &["agy".to_owned(), "codex".to_owned(), "messages".to_owned()]
        );
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

    #[test]
    fn flat_add_pane_preserves_in_order_dfs_traversal() {
        // Dynamic adds always land at the root level, matching the historical
        // flat `Vec<PaneId>` ordering.
        let mut layout = PaneLayout::new(SplitDirection::Vertical);
        layout.add_pane("a");
        layout.add_pane("b");
        layout.add_pane("c");

        assert_eq!(
            layout.panes(),
            &["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
        assert_eq!(layout.focused(), Some("a"));
    }

    #[test]
    fn nested_layout_rects_split_outer_then_inner() {
        // (top ― bottom) | right : left half split top/bottom, right half whole.
        let root = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            children: vec![
                LayoutChild::new(LayoutNode::Split {
                    direction: SplitDirection::Horizontal,
                    children: vec![
                        LayoutChild::new(LayoutNode::leaf("top")),
                        LayoutChild::new(LayoutNode::leaf("bottom")),
                    ],
                }),
                LayoutChild::new(LayoutNode::leaf("right")),
            ],
        };
        let layout = PaneLayout::from_root(root);

        let rects = layout.pane_rects(Rect::new(0, 0, 100, 24));

        assert_eq!(rects[0], ("top".to_owned(), Rect::new(0, 0, 50, 12)));
        assert_eq!(rects[1], ("bottom".to_owned(), Rect::new(0, 12, 50, 12)));
        assert_eq!(rects[2], ("right".to_owned(), Rect::new(50, 0, 50, 24)));
    }

    #[test]
    fn sized_children_split_by_ratio() {
        // agy:60 | codex:40
        let root = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            children: vec![
                LayoutChild::sized(LayoutNode::leaf("agy"), Some(60)),
                LayoutChild::sized(LayoutNode::leaf("codex"), Some(40)),
            ],
        };
        let layout = PaneLayout::from_root(root);

        let rects = layout.pane_rects(Rect::new(0, 0, 100, 24));

        assert_eq!(rects[0], ("agy".to_owned(), Rect::new(0, 0, 60, 24)));
        assert_eq!(rects[1], ("codex".to_owned(), Rect::new(60, 0, 40, 24)));
    }

    #[test]
    fn all_none_constraints_match_even_split() {
        let children = vec![
            LayoutChild::new(LayoutNode::leaf("a")),
            LayoutChild::new(LayoutNode::leaf("b")),
            LayoutChild::new(LayoutNode::leaf("c")),
        ];

        assert_eq!(child_constraints(&children), even_constraints(3));
    }

    #[test]
    fn all_some_ratios_normalize_to_one_hundred() {
        // ratios 1:1:2 over total 4 -> 25/25/50, remainder to leading.
        let children = vec![
            LayoutChild::sized(LayoutNode::leaf("a"), Some(1)),
            LayoutChild::sized(LayoutNode::leaf("b"), Some(1)),
            LayoutChild::sized(LayoutNode::leaf("c"), Some(2)),
        ];

        assert_eq!(
            child_constraints(&children),
            vec![
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(50),
            ]
        );
    }

    #[test]
    fn mixed_sized_and_unsized_fill_remaining_evenly() {
        // (a:50 | b) ― c at one level: a takes 50, b and c share the other 50.
        let children = vec![
            LayoutChild::sized(LayoutNode::leaf("a"), Some(50)),
            LayoutChild::new(LayoutNode::leaf("b")),
            LayoutChild::new(LayoutNode::leaf("c")),
        ];

        assert_eq!(
            child_constraints(&children),
            vec![
                Constraint::Percentage(50),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ]
        );
    }

    #[test]
    fn with_pending_pane_appends_root_leaf_for_sizing() {
        let layout = PaneLayout::with_panes(SplitDirection::Vertical, ["a"]);

        let pending = layout.with_pending_pane("__pending__");
        let rects = pending.pane_rects(Rect::new(0, 0, 100, 24));

        assert_eq!(rects.len(), 2);
        assert_eq!(rects[1].0, "__pending__");
        assert_eq!(rects[1].1, Rect::new(50, 0, 50, 24));
    }
}
