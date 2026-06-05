//! Feed a raw PTY byte capture into `TerminalParser` and report style state.
//!
//! Capture bytes from a live daemon via IPC (`event.subscribe` →
//! `client.attach` → concatenate `pty.output_chunk.bytes`), then run:
//!
//! ```sh
//! cargo run -p agentmux-terminal --example parse_raw_debug -- <raw-file> [rows] [cols]
//! ```
//!
//! Prints per-attribute cell counts and the final screen text so display bugs
//! (e.g. stuck SGR attributes) can be diagnosed against real agent output
//! instead of guessing from screenshots.

use agentmux_terminal::TerminalParser;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: parse_raw_debug <raw-file> [rows] [cols]");
        std::process::exit(2);
    });
    let rows: u16 = args.next().and_then(|v| v.parse().ok()).unwrap_or(37);
    let cols: u16 = args.next().and_then(|v| v.parse().ok()).unwrap_or(198);

    let bytes = std::fs::read(&path).expect("read raw capture file");
    let mut parser = TerminalParser::new(rows, cols);
    parser.advance(&bytes);

    let grid = parser.grid();
    let (mut printed, mut underline, mut dim, mut bold, mut italic, mut reverse) =
        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = grid.cell(row, col) else {
                continue;
            };
            if cell.ch == ' ' {
                continue;
            }
            printed += 1;
            underline += u32::from(cell.style.underline);
            dim += u32::from(cell.style.dim);
            bold += u32::from(cell.style.bold);
            italic += u32::from(cell.style.italic);
            reverse += u32::from(cell.style.reverse);
        }
    }

    println!("input: {path} ({} bytes), grid {rows}x{cols}", bytes.len());
    println!(
        "non-blank cells: {printed} | underline: {underline} | dim: {dim} | bold: {bold} | italic: {italic} | reverse: {reverse}"
    );
    println!("--- screen ---");
    for row in 0..rows {
        if let Some(line) = grid.line_text(row) {
            println!("{:>3}|{}", row, line.trim_end());
        }
    }
}
