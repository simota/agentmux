//! Copy-mode selection geometry.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopySelection {
    pub agent_id: String,
    pub start: CopyPoint,
    pub end: CopyPoint,
}

impl CopySelection {
    pub fn new(agent_id: impl Into<String>, start: CopyPoint, end: CopyPoint) -> Self {
        Self {
            agent_id: agent_id.into(),
            start,
            end,
        }
    }

    pub fn normalized(&self) -> (CopyPoint, CopyPoint) {
        if (self.end.row, self.end.col) < (self.start.row, self.start.col) {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (start, end) = self.normalized();
        if row < start.row || row > end.row {
            return false;
        }
        if start.row == end.row {
            return col >= start.col && col <= end.col;
        }
        if row == start.row {
            return col >= start.col;
        }
        if row == end.row {
            return col <= end.col;
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyPoint {
    pub row: u16,
    pub col: u16,
}
