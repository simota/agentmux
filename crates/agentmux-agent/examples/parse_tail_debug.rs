//! Temporary debug harness: run the result marker parser against a captured
//! raw PTY tail file. Usage: cargo run -p agentmux-agent --example parse_tail_debug -- <path>
use agentmux_agent::result::{AgentResultParse, parse_agent_result_marker};

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: parse_tail_debug <path>");
    let tail = std::fs::read_to_string(&path).expect("read tail file");
    match parse_agent_result_marker(&tail) {
        AgentResultParse::Found(parsed) => {
            println!("FOUND offset={}", parsed.marker_offset);
            println!("summary: {}", parsed.result.summary);
            for m in &parsed.result.messages {
                println!("message to={} kind={:?} body={}", m.to, m.kind, m.body);
            }
        }
        AgentResultParse::NotFound => println!("NOT_FOUND"),
        AgentResultParse::NeedsStatusProbe(p) => {
            println!(
                "NEEDS_STATUS_PROBE offset={} reason={}",
                p.marker_offset, p.reason
            );
        }
    }
}
