use crate::grid::{CellStyle, CursorState, ScreenGrid, TerminalColor};

/// Currently visible terminal screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveScreen {
    Primary,
    Alternate,
}

/// Incremental VTE-backed terminal buffer.
pub struct TerminalParser {
    inner: vte::Parser,
    primary: ScreenGrid,
    alternate: ScreenGrid,
    active_screen: ActiveScreen,
    current_style: CellStyle,
    saved_primary_cursor: CursorState,
    saved_alternate_cursor: CursorState,
    title: Option<String>,
    bracketed_paste: bool,
}

impl TerminalParser {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            inner: vte::Parser::new(),
            primary: ScreenGrid::new(rows, cols),
            alternate: ScreenGrid::new(rows, cols),
            active_screen: ActiveScreen::Primary,
            current_style: CellStyle::default(),
            saved_primary_cursor: CursorState::default(),
            saved_alternate_cursor: CursorState::default(),
            title: None,
            bracketed_paste: false,
        }
    }

    /// Feed a chunk of raw PTY bytes into the active screen.
    pub fn advance(&mut self, bytes: &[u8]) {
        let Self {
            inner,
            primary,
            alternate,
            active_screen,
            current_style,
            saved_primary_cursor,
            saved_alternate_cursor,
            title,
            bracketed_paste,
        } = self;

        let mut performer = GridPerformer {
            primary,
            alternate,
            active_screen,
            current_style,
            saved_primary_cursor,
            saved_alternate_cursor,
            title,
            bracketed_paste,
        };

        for &byte in bytes {
            inner.advance(&mut performer, byte);
        }
    }

    pub fn grid(&self) -> &ScreenGrid {
        match self.active_screen {
            ActiveScreen::Primary => &self.primary,
            ActiveScreen::Alternate => &self.alternate,
        }
    }

    pub fn grid_mut(&mut self) -> &mut ScreenGrid {
        match self.active_screen {
            ActiveScreen::Primary => &mut self.primary,
            ActiveScreen::Alternate => &mut self.alternate,
        }
    }

    pub fn active_screen(&self) -> ActiveScreen {
        self.active_screen
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.primary.resize(rows, cols);
        self.alternate.resize(rows, cols);
    }
}

impl Default for TerminalParser {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

struct GridPerformer<'a> {
    primary: &'a mut ScreenGrid,
    alternate: &'a mut ScreenGrid,
    active_screen: &'a mut ActiveScreen,
    current_style: &'a mut CellStyle,
    saved_primary_cursor: &'a mut CursorState,
    saved_alternate_cursor: &'a mut CursorState,
    title: &'a mut Option<String>,
    bracketed_paste: &'a mut bool,
}

impl GridPerformer<'_> {
    fn grid(&mut self) -> &mut ScreenGrid {
        match self.active_screen {
            ActiveScreen::Primary => self.primary,
            ActiveScreen::Alternate => self.alternate,
        }
    }

    fn param(params: &vte::Params, index: usize, default: u16) -> u16 {
        params
            .iter()
            .nth(index)
            .and_then(|param| param.first().copied())
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }

    fn params(params: &vte::Params) -> Vec<u16> {
        if params.is_empty() {
            return vec![0];
        }

        params
            .iter()
            .flat_map(|param| {
                if param.is_empty() {
                    vec![0]
                } else {
                    param.to_vec()
                }
            })
            .collect()
    }

    fn set_mode(&mut self, params: &vte::Params, intermediates: &[u8], enabled: bool) {
        let is_private = intermediates == [b'?'];
        for param in Self::params(params) {
            match (is_private, param) {
                (true, 25) => self.grid().set_cursor_visible(enabled),
                (true, 1047 | 1049) if enabled => {
                    *self.active_screen = ActiveScreen::Alternate;
                    self.alternate.clear_screen();
                }
                (true, 1047 | 1049) => {
                    *self.active_screen = ActiveScreen::Primary;
                }
                (true, 2004) => *self.bracketed_paste = enabled,
                _ => {}
            }
        }
    }

    fn apply_sgr(&mut self, params: &vte::Params) {
        let values = Self::params(params);
        let mut index = 0;

        while index < values.len() {
            match values[index] {
                0 => *self.current_style = CellStyle::default(),
                1 => self.current_style.bold = true,
                2 => self.current_style.dim = true,
                3 => self.current_style.italic = true,
                4 => self.current_style.underline = true,
                7 => self.current_style.reverse = true,
                22 => {
                    self.current_style.bold = false;
                    self.current_style.dim = false;
                }
                23 => self.current_style.italic = false,
                24 => self.current_style.underline = false,
                27 => self.current_style.reverse = false,
                30..=37 => {
                    self.current_style.fg =
                        Some(TerminalColor::Indexed((values[index] - 30) as u8));
                }
                39 => self.current_style.fg = None,
                40..=47 => {
                    self.current_style.bg =
                        Some(TerminalColor::Indexed((values[index] - 40) as u8));
                }
                49 => self.current_style.bg = None,
                90..=97 => {
                    self.current_style.fg =
                        Some(TerminalColor::Indexed(8 + (values[index] - 90) as u8));
                }
                100..=107 => {
                    self.current_style.bg =
                        Some(TerminalColor::Indexed(8 + (values[index] - 100) as u8));
                }
                38 | 48 => {
                    let is_fg = values[index] == 38;
                    if let Some((color, consumed)) = parse_extended_color(&values[index + 1..]) {
                        if is_fg {
                            self.current_style.fg = Some(color);
                        } else {
                            self.current_style.bg = Some(color);
                        }
                        index += consumed;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn saved_cursor(&mut self) -> &mut CursorState {
        match self.active_screen {
            ActiveScreen::Primary => self.saved_primary_cursor,
            ActiveScreen::Alternate => self.saved_alternate_cursor,
        }
    }

    fn save_cursor(&mut self) {
        *self.saved_cursor() = self.grid().cursor();
    }

    fn restore_cursor(&mut self) {
        let cursor = *self.saved_cursor();
        self.grid().set_cursor(cursor.row, cursor.col);
        self.grid().set_cursor_visible(cursor.visible);
    }

    fn reverse_index(&mut self) {
        let cursor = self.grid().cursor();
        let top = self.grid().scroll_region().map_or(0, |(top, _)| top);
        if cursor.row == top {
            let bottom = self.grid().scroll_region().map_or(0, |(_, bottom)| bottom);
            self.grid().scroll_down_region(top, bottom, 1);
        } else {
            self.grid().move_cursor(-1, 0);
        }
    }

    fn set_scroll_region(&mut self, params: &vte::Params) {
        let top = Self::param(params, 0, 1).saturating_sub(1);
        let bottom = if params.is_empty() || params.iter().nth(1).is_none() {
            self.grid().rows().saturating_sub(1)
        } else {
            Self::param(params, 1, self.grid().rows()).saturating_sub(1)
        };
        self.grid().set_scroll_region(top, bottom);
    }
}

impl vte::Perform for GridPerformer<'_> {
    fn print(&mut self, c: char) {
        let style = self.current_style.clone();
        self.grid().write_char(c, style);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | b'\r' | 0x08 => self.grid().write_char(byte as char, CellStyle::default()),
            b'\t' => {
                let next_tab = ((self.grid().cursor().col / 8) + 1) * 8;
                let spaces = next_tab.saturating_sub(self.grid().cursor().col).max(1);
                for _ in 0..spaces {
                    self.print(' ');
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(kind) = params.first() else {
            return;
        };

        if matches!(*kind, b"0" | b"2") {
            if let Some(raw_title) = params.get(1) {
                *self.title = Some(String::from_utf8_lossy(raw_title).into_owned());
            }
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore {
            return;
        }

        match action {
            '@' => self.grid().insert_blank_chars(Self::param(params, 0, 1)),
            'A' => self
                .grid()
                .move_cursor(-(Self::param(params, 0, 1) as i16), 0),
            'B' => self.grid().move_cursor(Self::param(params, 0, 1) as i16, 0),
            'C' => self.grid().move_cursor(0, Self::param(params, 0, 1) as i16),
            'D' => self
                .grid()
                .move_cursor(0, -(Self::param(params, 0, 1) as i16)),
            'G' => {
                let col = Self::param(params, 0, 1).saturating_sub(1);
                let row = self.grid().cursor().row;
                self.grid().set_cursor(row, col);
            }
            'H' | 'f' => {
                let row = Self::param(params, 0, 1).saturating_sub(1);
                let col = Self::param(params, 1, 1).saturating_sub(1);
                self.grid().set_cursor(row, col);
            }
            'J' => match Self::param(params, 0, 0) {
                0 => self.grid().clear_screen_from_cursor(),
                1 => self.grid().clear_screen_to_cursor(),
                2 | 3 => self.grid().clear_screen(),
                _ => {}
            },
            'K' => match Self::param(params, 0, 0) {
                0 => self.grid().clear_line_from_cursor(),
                1 => self.grid().clear_line_to_cursor(),
                2 => {
                    let row = self.grid().cursor().row;
                    self.grid().clear_line(row);
                }
                _ => {}
            },
            'L' => self.grid().insert_blank_lines(Self::param(params, 0, 1)),
            'M' => self.grid().delete_lines(Self::param(params, 0, 1)),
            'P' => self.grid().delete_chars(Self::param(params, 0, 1)),
            'S' => self.grid().scroll_up(Self::param(params, 0, 1)),
            'T' => self.grid().scroll_down(Self::param(params, 0, 1)),
            'X' => self.grid().erase_chars(Self::param(params, 0, 1)),
            'r' => self.set_scroll_region(params),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            'm' => self.apply_sgr(params),
            'h' => self.set_mode(params, intermediates, true),
            'l' => self.set_mode(params, intermediates, false),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }

        match (intermediates, byte) {
            ([], b'c') => {
                self.primary.clear_screen();
                self.alternate.clear_screen();
                self.primary.reset_scroll_region();
                self.alternate.reset_scroll_region();
                *self.active_screen = ActiveScreen::Primary;
                *self.current_style = CellStyle::default();
                *self.saved_primary_cursor = CursorState::default();
                *self.saved_alternate_cursor = CursorState::default();
                *self.title = None;
                *self.bracketed_paste = false;
            }
            ([], b'D') => self.execute(b'\n'),
            ([], b'E') => {
                self.execute(b'\r');
                self.execute(b'\n');
            }
            ([], b'M') => self.reverse_index(),
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            _ => {}
        }
    }
}

fn parse_extended_color(values: &[u16]) -> Option<(TerminalColor, usize)> {
    match values {
        [5, index, ..] => Some((TerminalColor::Indexed((*index).min(255) as u8), 2)),
        [2, r, g, b, ..] => Some((
            TerminalColor::Rgb {
                r: (*r).min(255) as u8,
                g: (*g).min(255) as u8,
                b: (*b).min(255) as u8,
            },
            4,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_text_and_controls_update_grid() {
        let mut parser = TerminalParser::new(2, 8);

        parser.advance(b"ab\tc\rZ\nxy");

        assert_eq!(parser.grid().line_text(0).as_deref(), Some("Z       "));
        assert_eq!(parser.grid().line_text(1).as_deref(), Some("xy      "));
    }

    #[test]
    fn csi_cursor_clear_and_sgr_update_grid() {
        let mut parser = TerminalParser::new(3, 6);

        parser.advance(b"abcdef\x1b[2;3H\x1b[31;1mZ\x1b[K");

        assert_eq!(parser.grid().line_text(0).as_deref(), Some("abcdef"));
        assert_eq!(parser.grid().line_text(1).as_deref(), Some("  Z   "));
        let cell = parser.grid().cell(1, 2).expect("styled cell");
        assert_eq!(cell.style.fg, Some(TerminalColor::Indexed(1)));
        assert!(cell.style.bold);
    }

    #[test]
    fn csi_clear_from_wide_continuation_keeps_grid_aligned() {
        let mut parser = TerminalParser::new(1, 8);

        parser.advance("ab変\x1b[D\x1b[Kc".as_bytes());

        assert_eq!(parser.grid().line_text(0).as_deref(), Some("ab c    "));
    }

    #[test]
    fn ime_style_line_redraw_with_wide_text_keeps_display_width() {
        let mut parser = TerminalParser::new(1, 8);

        parser.advance("abc\r\x1b[K変換".as_bytes());

        assert_eq!(parser.grid().cursor().col, 4);
        assert_eq!(parser.grid().line_text(0).as_deref(), Some("変換    "));
    }

    #[test]
    fn alternate_screen_is_separate_from_primary() {
        let mut parser = TerminalParser::new(2, 5);

        parser.advance(b"main\x1b[?1049halt\x1b[2Jalt\x1b[?1049l");

        assert_eq!(parser.active_screen(), ActiveScreen::Primary);
        assert_eq!(parser.grid().line_text(0).as_deref(), Some("main "));

        parser.advance(b"\x1b[?1049halt");
        assert_eq!(parser.active_screen(), ActiveScreen::Alternate);
        assert_eq!(parser.grid().line_text(0).as_deref(), Some("alt  "));
    }

    #[test]
    fn osc_title_and_bracketed_paste_mode_are_tracked() {
        let mut parser = TerminalParser::new(1, 5);

        parser.advance(b"\x1b]2;agentmux\x07\x1b[?2004h");

        assert_eq!(parser.title(), Some("agentmux"));
        assert!(parser.bracketed_paste());

        parser.advance(b"\x1b[?2004l");

        assert!(!parser.bracketed_paste());
    }

    #[test]
    fn decstbm_limits_linefeed_scrolling_to_scroll_region() {
        let mut parser = TerminalParser::new(4, 4);

        parser.advance(b"\x1b[1;1Haaaa\x1b[2;1Hbbbb\x1b[3;1Hcccc\x1b[4;1Hdddd");
        parser.advance(b"\x1b[2;3r\x1b[3;1H\n");

        assert_eq!(parser.grid().scroll_region(), Some((1, 2)));
        assert_eq!(parser.grid().line_text(0).as_deref(), Some("aaaa"));
        assert_eq!(parser.grid().line_text(1).as_deref(), Some("cccc"));
        assert_eq!(parser.grid().line_text(2).as_deref(), Some("    "));
        assert_eq!(parser.grid().line_text(3).as_deref(), Some("dddd"));
    }

    #[test]
    fn reverse_index_scrolls_down_inside_scroll_region() {
        let mut parser = TerminalParser::new(4, 4);

        parser.advance(b"\x1b[1;1Haaaa\x1b[2;1Hbbbb\x1b[3;1Hcccc\x1b[4;1Hdddd");
        parser.advance(b"\x1b[2;3r\x1b[2;1H\x1bM");

        assert_eq!(parser.grid().line_text(0).as_deref(), Some("aaaa"));
        assert_eq!(parser.grid().line_text(1).as_deref(), Some("    "));
        assert_eq!(parser.grid().line_text(2).as_deref(), Some("bbbb"));
        assert_eq!(parser.grid().line_text(3).as_deref(), Some("dddd"));
    }

    #[test]
    fn csi_and_esc_cursor_save_restore_are_supported() {
        let mut parser = TerminalParser::new(3, 5);

        parser.advance(b"\x1b[2;3H\x1b[s\x1b[1;1H\x1b[uA");
        parser.advance(b"\x1b[3;5H\x1b7\x1b[1;1H\x1b8B");

        assert_eq!(parser.grid().line_text(1).as_deref(), Some("  A  "));
        assert_eq!(parser.grid().line_text(2).as_deref(), Some("    B"));
    }

    #[test]
    fn csi_scroll_down_inserts_blank_lines_at_top() {
        let mut parser = TerminalParser::new(3, 3);

        parser.advance(b"\x1b[1;1Haaa\x1b[2;1Hbbb\x1b[3;1Hccc\x1b[1T");

        assert_eq!(parser.grid().line_text(0).as_deref(), Some("   "));
        assert_eq!(parser.grid().line_text(1).as_deref(), Some("aaa"));
        assert_eq!(parser.grid().line_text(2).as_deref(), Some("bbb"));
    }

    #[test]
    fn csi_character_insert_delete_and_erase_update_line() {
        let mut parser = TerminalParser::new(1, 6);

        parser.advance(b"abcdef\x1b[3G\x1b[2X");
        assert_eq!(parser.grid().line_text(0).as_deref(), Some("ab  ef"));

        parser.advance(b"\rabcdef\x1b[3G\x1b[2P");
        assert_eq!(parser.grid().line_text(0).as_deref(), Some("abef  "));

        parser.advance(b"\rabcdef\x1b[3G\x1b[2@");
        assert_eq!(parser.grid().line_text(0).as_deref(), Some("ab  cd"));
    }

    #[test]
    fn csi_line_insert_delete_respects_scroll_region() {
        let mut parser = TerminalParser::new(4, 4);

        parser.advance(b"\x1b[1;1Haaaa\x1b[2;1Hbbbb\x1b[3;1Hcccc\x1b[4;1Hdddd");
        parser.advance(b"\x1b[2;4r\x1b[3;1H\x1b[L");

        assert_eq!(parser.grid().line_text(0).as_deref(), Some("aaaa"));
        assert_eq!(parser.grid().line_text(1).as_deref(), Some("bbbb"));
        assert_eq!(parser.grid().line_text(2).as_deref(), Some("    "));
        assert_eq!(parser.grid().line_text(3).as_deref(), Some("cccc"));

        parser.advance(b"\x1b[2;1H\x1b[M");

        assert_eq!(parser.grid().line_text(0).as_deref(), Some("aaaa"));
        assert_eq!(parser.grid().line_text(1).as_deref(), Some("    "));
        assert_eq!(parser.grid().line_text(2).as_deref(), Some("cccc"));
        assert_eq!(parser.grid().line_text(3).as_deref(), Some("    "));
    }
}
