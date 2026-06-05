//! Print an `agent.send_input_script` JSON payload for debugging.
//!
//! Used by ad-hoc reproduction drivers (e.g. live PTY editing-bug captures)
//! that talk JSONL to the daemon from outside Rust and cannot easily
//! construct `InputScript` (ULID ids + `time` serde formats) by hand.
//!
//! ```sh
//! cargo run -p agentmux-agent --example print_input_script -- \
//!     <agent_id> "text:hello|bs|raw:1b5b44|enter"
//! ```
//!
//! Step DSL (pipe-separated): `text:<s>`, `raw:<hex>`, `bs`, `enter`, `esc`, `tab`.

use agentmux_agent::{InputAction, InputScript, adapter::InputSafety};
use agentmux_core::{DateTimeUtc, InputScriptId};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(agent_id), Some(spec)) = (args.next(), args.next()) else {
        eprintln!("usage: print_input_script <agent_id> <step|step|...>");
        std::process::exit(2);
    };

    let actions: Vec<InputAction> = spec
        .split('|')
        .map(|step| {
            let step = step.trim();
            if let Some(text) = step.strip_prefix("text:") {
                InputAction::TypeText(text.to_string())
            } else if let Some(hex) = step.strip_prefix("raw:") {
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex byte"))
                    .collect();
                InputAction::SendRaw(bytes)
            } else {
                match step {
                    "bs" => InputAction::PressBackspace,
                    "enter" => InputAction::PressEnter,
                    "esc" => InputAction::PressEsc,
                    "tab" => InputAction::PressTab,
                    other => panic!("unknown step '{other}'"),
                }
            }
        })
        .collect();

    let script = InputScript {
        id: InputScriptId::new(),
        target_agent_id: agent_id.parse().expect("valid agent id"),
        reason: "debug reproduction input".to_string(),
        preconditions: Vec::new(),
        actions,
        safety: InputSafety::Safe,
        created_at: DateTimeUtc::now_utc(),
    };

    println!(
        "{}",
        serde_json::to_string(&script).expect("InputScript serializes")
    );
}
