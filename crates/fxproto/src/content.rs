//! Normalized content shapes — what a message is made of after translation from ACP.
//!
//! Rule (from architecture.md): payloads never embed raw ACP JSON. Vendor extras ride
//! opaquely in `_meta: Option<serde_json::Value>` fields where needed.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One block of user/agent content. Mirrors ACP content blocks, minus the parts
/// we normalize away. Internally tagged so JSON reads `{"type": "text", "text": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        data: String, /* base64 */
    },
    /// Inline file/resource an agent embeds instead of plain text — the ACP
    /// "resource" content block (MCP EmbeddedResource shape). Splits at our seam:
    /// `uri` + `media_type` are scalars; the payload is either UTF-8 text or raw
    /// base64, distinguished ON THE WIRE by the tagged `contents` wrapper so
    /// consumers never guess from the mime type.
    Resource {
        uri: String,
        media_type: String,
        contents: ResourceContents,
    },
}
//     media_type stays a plain String (mime strings are open-ended; an enum would churn
//     against agent vendors). Eq/PartialEq: golden tests assert structural equality.

/// Payload of [`ContentBlock::Resource`] — maps ACP's TextResourceContents /
/// BlobResourceContents pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResourceContents {
    Text { text: String },
    Blob { blob: String }, // base64
}

/// Speaker of a Chunk. Thought-chunks are deliberately NOT a Role yet: their shape
/// is an open decision parked in fxcore normalize.rs ("own variant vs Chunk w/ role?
/// lean: defer") and in event.rs's trailing TODO. Keep Role two-variant until real
/// ACP traffic settles it; revisit at M1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Agent,
}

/// Why a turn stopped — mirrors ACP v1 stopReason 1:1 so nothing is lost in
/// translation. normalize.rs owns the exhaustive acp::StopReason → StopReason match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

/// An MCP server the CLIENT wants attached to a session. Sent in Command::NewSession;
/// echoed verbatim by FxEvent::SessionCreated so replays rebuild what was attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>, // BTreeMap: byte-stable wire ordering for goldens
}
//     NOTE canonical name: McpServerSpec. docs/architecture.md sketches `McpServer` —
//     stale shorthand; crates.md + this file + command.rs all say McpServerSpec.

/// One row of an agent's plan. priority mirrors ACP's plan-entry priority enum;
/// Option because agents may omit it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanEntryStatus,
    pub priority: Option<PlanPriority>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}
//     → these mirror ACP kinds/statuses 1:1; keep variants aligned with ACP v1 schema
//       (normalize.rs's exhaustive matches break compile if ACP grows a variant we lack).

// Serde summary for goldens: ContentBlock = internally tagged "type"; every other enum =
// externally tagged unit variants rendered as bare snake_case strings ("end_turn",
// "in_progress", ...). Structs = plain field maps.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_wire_shape() {
        let text = serde_json::to_string(&ContentBlock::Text { text: "hi".into() }).unwrap();
        assert_eq!(text, r#"{"type":"text","text":"hi"}"#);
        let img = serde_json::to_string(&ContentBlock::Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        })
        .unwrap();
        assert_eq!(
            img,
            r#"{"type":"image","media_type":"image/png","data":"AAAA"}"#
        );
    }

    #[test]
    fn embedded_resource_wire_shape() {
        let file = ContentBlock::Resource {
            uri: "file:///src/main.rs".into(),
            media_type: "text/x-rust".into(),
            contents: ResourceContents::Text {
                text: "fn main() {}".into(),
            },
        };
        let json = serde_json::to_string(&file).unwrap();
        assert_eq!(
            json,
            concat!(
                r#"{"type":"resource","uri":"file:///src/main.rs","media_type":"text/x-rust","#,
                r#""contents":{"type":"text","text":"fn main() {}"}}"#,
            )
        );
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, file);

        let bin = ContentBlock::Resource {
            uri: "file:///a.bin".into(),
            media_type: "application/octet-stream".into(),
            contents: ResourceContents::Blob {
                blob: "AAEC".into(),
            },
        };
        let json = serde_json::to_string(&bin).unwrap();
        assert!(
            json.contains(r#""contents":{"type":"blob","blob":"AAEC"}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<ContentBlock>(&json).unwrap(), bin);
    }

    #[test]
    fn unit_enums_are_bare_snake_case_strings() {
        let cases: Vec<(String, &str)> = vec![
            (serde_json::to_string(&Role::User).unwrap(), "\"user\""),
            (serde_json::to_string(&Role::Agent).unwrap(), "\"agent\""),
            (
                serde_json::to_string(&StopReason::EndTurn).unwrap(),
                "\"end_turn\"",
            ),
            (
                serde_json::to_string(&StopReason::MaxTokens).unwrap(),
                "\"max_tokens\"",
            ),
            (
                serde_json::to_string(&StopReason::MaxTurnRequests).unwrap(),
                "\"max_turn_requests\"",
            ),
            (
                serde_json::to_string(&StopReason::Refusal).unwrap(),
                "\"refusal\"",
            ),
            (
                serde_json::to_string(&StopReason::Cancelled).unwrap(),
                "\"cancelled\"",
            ),
            (
                serde_json::to_string(&PlanEntryStatus::InProgress).unwrap(),
                "\"in_progress\"",
            ),
            (
                serde_json::to_string(&ToolCallKind::Execute).unwrap(),
                "\"execute\"",
            ),
            (
                serde_json::to_string(&ToolCallStatus::Failed).unwrap(),
                "\"failed\"",
            ),
            (
                serde_json::to_string(&PlanPriority::High).unwrap(),
                "\"high\"",
            ),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
        }
    }

    #[test]
    fn mcp_spec_round_trips_with_stable_env_order() {
        let mut env = BTreeMap::new();
        env.insert("B_KEY".to_string(), "2".to_string());
        env.insert("A_KEY".to_string(), "1".to_string());
        let spec = McpServerSpec {
            name: "fs".into(),
            command: "mcp-fs".into(),
            args: vec!["--root".into(), "/tmp".into()],
            env,
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(
            json.contains("\"A_KEY\":\"1\",\"B_KEY\":\"2\""),
            "env keys must serialize in BTreeMap order: {json}"
        );
        let back: McpServerSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(back, spec);
    }
}
