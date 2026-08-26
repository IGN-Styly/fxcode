//! Thread projection: the transcript of one session. This is what the UI renders.
//!
//! Model: `messages` (Vec, append-only) + `tool_calls` (BTreeMap upserts) + `flow`
//! (Vec<FlowItem> render order). Every one of the NINE FxEvent variants from
//! event.rs is mapped below — nothing is "obvious", including SessionCreated, which
//! this file owns the client-side payload of.

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
// use std::path::PathBuf;
//
// use serde::{Deserialize, Serialize};
// use serde_json::Value;
// use tracing::{debug, warn};
//
// use crate::content::{McpServerSpec, PlanEntry, Role, ToolCallKind, ToolCallStatus};
// use crate::event::FxEvent;
// use crate::ids::{OptionId, RequestId, SessionId, ToolCallId, TurnId};

// TODO: define:
//
// /// Derives REQUIRED (model/mod.rs derive rule): Serialize + Deserialize because
// /// envelope.rs Snapshot serializes ThreadsState whole; Default where marked;
// /// Clone/Debug/PartialEq per checklist.
// #[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct ThreadsState {
//     /// BTreeMap: deterministic iteration + byte-stable snapshots; SessionIds are
//     /// uuid v7 so iteration == creation order.
//     pub threads: BTreeMap<SessionId, ThreadState>,
// }
//
// #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
// pub struct ThreadState {
//     /// From SessionCreated — the ONLY home for these client-side (agents.rs keeps
//     /// no session metadata). Set on create; a re-emitted SessionCreated
//     /// overwrites unconditionally (same data by construction).
//     pub cwd: PathBuf,
//     pub mcp_servers: Vec<McpServerSpec>,
//
//     /// Transcript in arrival order. APPEND-ONLY: never remove, reorder or compact,
//     /// so every FlowItem::Message index stays valid forever.
//     pub messages: Vec<Message>,
//     /// Keyed upsert target — ToolCallUpsert replaces by id, never appends dupes.
//     pub tool_calls: BTreeMap<ToolCallId, ToolCall>,
//     /// Render order interleaving messages and cards; views/thread.rs walks this
//     /// as ONE flat VirtualList. See W3 for when items enter it.
//     pub flow: Vec<FlowItem>,
//     /// Replaced wholesale by each PlanUpdated (rule W4). Not merged.
//     pub plan: Vec<PlanEntry>,
//     /// Set by TurnStarted, cleared by the MATCHING TurnFinished (rule W7).
//     /// Server guarantees at most one active turn per session (Prompt while active
//     /// => Reply::TurnNotActive); the fold still guards against violations.
//     pub active_turn: Option<TurnId>,
//     /// request_id -> tool_call bridge so PermissionResolved can annotate the card
//     /// it belonged to (rules W5/W6). Populated by PermissionRequested, drained by
//     /// PermissionResolved.
//     pub pending_perm_tools: BTreeMap<RequestId, ToolCallId>,
// }
//
// #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct Message { pub role: Role, pub text: String }
//
// #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub struct ToolCall {
//     pub title: String,
//     pub kind: ToolCallKind,
//     pub status: ToolCallStatus,
//     pub output: Option<String>,
//     /// Opaque vendor extras, passed through from ToolCallUpsert._meta.
//     pub _meta: Option<Value>,
//     /// Stamped when the permission over this tool resolves (W6). None until then;
//     /// None means "never asked", which is exactly why PermOutcome exists instead
//     /// of a bare Option<OptionId>.
//     pub perm: Option<PermOutcome>,
// }
//
// /// Tri-state outcome for a tool card. Chosen carries whichever option id the user
// /// picked (allow AND reject variants alike — the UI renders the name from its own
// /// sources); Cancelled covers dismiss, watchdog timeout and turn-cancel sweep.
// #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// pub enum PermOutcome { Chosen(OptionId), Cancelled }
//
// /// usize indexes into `messages` — stable because messages is append-only.
// /// Derive Clone, Debug, PartialEq, Serialize, Deserialize.
// pub enum FlowItem { Message(usize), Tool(ToolCallId) }
//     ← lets thread.rs render one flat VirtualList over messages + cards in true order.
//
// /// Fold rules — EXACT trigger map, all nine variants. "ensure(S)" = get-or-create
// /// threads[S] with ThreadState::default(); creating from anything other than
// /// SessionCreated logs debug! (means replay started mid-session / snapshot cut).
// ///
// ///   SessionCreated { session, agent, cwd, mcp_servers }
// ///     W0  ensure(session); overwrite cwd + mcp_servers with event values. Nothing
// ///         else — the agent linkage lives in AgentsState, never duplicated here.
// ///
// ///   TurnStarted { session, turn }
// ///     W1  ensure; active_turn = Some(turn). Overwriting a DIFFERENT existing Some
// ///         => warn! (protocol violation) but proceed last-writer-wins; overwriting
// ///         None is the normal path.
// ///
// ///   Chunk { session, turn, role, text }
// ///     W2  ensure; MERGE ALGORITHM — inspect flow.last() ONLY:
// ///           Some(FlowItem::Message(i)) AND messages[i].role == role
// ///               => messages[i].text.push_str(text);
// ///           otherwise (empty flow | last item is Tool | role differs)
// ///               => messages.push(Message { role, text: text });
// ///                  flow.push(FlowItem::Message(messages.len() - 1));
// ///         Never search backwards past a tool card: streaming text must not jump
// ///         above the tool that interrupted it. `turn` is accepted but NOT stored —
// ///         the transcript is continuous across turns. Empty-text chunks follow the
// ///         same path (a merge may append "" — harmless). Chunk is the ONE variant
// ///         that is not re-apply-safe; exactly-once delivery covers it (mod.rs).
// ///
// ///   ToolCallUpsert { session, tool_call, title, kind, status, output, _meta }
// ///     W3  ensure; UPSERT map entry — all six payload fields replaced wholesale by
// ///         the event (no field-wise merge), EXCEPT `perm`, which is preserved
// ///         from the existing entry if any. First appearance of this id: ALSO
// ///         flow.push(FlowItem::Tool(tool_call)); later upserts NEVER touch flow
// ///         (position = first appearance). Arriving BEFORE any message is normal
// ///         (tool-first turns): flow simply starts with a Tool item; no synthetic
// ///         user message is invented.
// ///
// ///   PlanUpdated { session, entries }
// ///     W4  ensure; plan = entries. REPLACE wholesale — each event is a full plan
// ///         snapshot (ACP semantics); merging by index/priority would resurrect
// ///         deleted entries.
// ///
// ///   PermissionRequested { request_id, session, tool_call, options }
// ///     W5  ensure; pending_perm_tools.insert(request_id, tool_call.tool_call).
// ///         Options are NOT copied here (perms.rs owns them); this map is only the
// ///         id bridge. Duplicate request_id => overwrite + debug log.
// ///
// ///   PermissionResolved { request_id, chosen }
// ///     W6  NO ensure — this variant NEVER creates a thread. Take pending_perm_
// ///         tools.remove(request_id); absent => debug log, done. Found AND
// ///         tool_calls contains the id => set that ToolCall.perm =
// ///             Some(match chosen { Some(id) => PermOutcome::Chosen(id),
// ///                                 None      => PermOutcome::Cancelled }).
// ///         Found but tool never upserted => drop silently (debug log): there is
// ///         no card to annotate, and inventing one is out of scope.
// ///
// ///   TurnFinished { session, turn, stop_reason }
// ///     W7  ensure; if active_turn == Some(turn) => clear to None. Mismatched turn
// ///         or already-None => warn! + no-op, so DOUBLE TurnFinished and stale
// ///         finishes are absorbed idempotently. stop_reason is NOT stored in v0
// ///         (no UI consumer yet; revisit at M4 checkpoints). The transcript
// ///         persists across turns — nothing resets between turns.
// ///
// ///   AgentStatus
// ///     W8  ignore (agents.rs owns agent lifecycle entirely).
// ///
// /// Ownership summary: apply_thread is the only writer of ThreadsState on either
// /// side. For cancel flows the server emits PermissionResolved { chosen: None }
// /// per swept request plus TurnFinished { stop_reason: Cancelled }; this fold and
// /// perms.rs each react to those events independently — perms.rs owns the audit
// /// row ("cancelled" in recent), threads.rs owns the card badge (PermOutcome::
// /// Cancelled). Neither reads the other's state, so their relative arrival order
// /// is irrelevant.
// pub fn apply_thread(state: &mut ThreadsState, ev: &FxEvent);
//
// Test checklist: model/mod.rs block "threads (apply_thread)", items T1–T17.
