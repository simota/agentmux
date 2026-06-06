//! Message-bus list item projected from daemon payloads.

use serde_json::Value;

use super::feed::{endpoint_label, string_field};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageListItem {
    pub message_id: String,
    pub created_at: String,
    pub delivery_status: String,
    pub kind: String,
    pub thread_id: Option<String>,
    pub from: String,
    pub to: String,
    pub body: String,
}

impl MessageListItem {
    pub(crate) fn from_payload(payload: &Value) -> Option<Self> {
        let message_id = string_field(payload, "message_id")?;
        Some(Self {
            message_id,
            created_at: string_field(payload, "created_at").unwrap_or_else(|| "-".to_string()),
            delivery_status: string_field(payload, "delivery_status")
                .unwrap_or_else(|| "-".to_string()),
            kind: string_field(payload, "kind").unwrap_or_else(|| "-".to_string()),
            thread_id: string_field(payload, "thread_id"),
            from: endpoint_label(payload.get("from")),
            to: endpoint_label(payload.get("to")),
            body: string_field(payload, "body").unwrap_or_default(),
        })
    }
}
