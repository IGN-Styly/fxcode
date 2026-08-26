//! Thread projection: the transcript of one session. This is what the UI renders.
//!
//! Model: `messages` (Vec, append-only) + `tool_calls` (BTreeMap upserts) + `flow`
//! (Vec<FlowItem> render order). Every one of the NINE FxEvent variants from
//! event.rs is mapped below — nothing is "obvious", including SessionCreated, which
//! this file owns the client-side payload of.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::{McpServerSpec, PlanEntry, Role, ToolCallKind, ToolCallStatus};
use crate::event::FxEvent;
use crate::ids::{OptionId, RequestId, SessionId, ToolCallId, TurnId};

/// Derives REQUIRED (model/mod.rs derive rule): Serialize + Deserialize because
/// envelope.rs Snapshot serializes ThreadsState whole.
#[derive(Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadsState {
    /// BTreeMap: deterministic iteration + byte-stable snapshots; SessionIds are
    /// uuid v7 so iteration == creation order.
    pub threads: BTreeMap<SessionId, ThreadState>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThreadState {
    /// From SessionCreated — the ONLY home for these client-side (agents.rs keeps
    /// no session metadata). Set on create; a re-emitted SessionCreated
    /// overwrites unconditionally (same data by construction).
    pub cwd: PathBuf,
    pub mcp_servers: Vec<McpServerSpec>,

    /// Transcript in arrival order. APPEND-ONLY: never remove, reorder or compact,
    /// so every FlowItem::Message index stays valid forever.
    pub messages: Vec<Message>,
    /// Keyed upsert target — ToolCallUpsert replaces by id, never appends dupes.
    pub tool_calls: BTreeMap<ToolCallId, ToolCall>,
    /// Render order interleaving messages and cards; views/thread.rs walks this
    /// as ONE flat VirtualList. See W3 for when items enter it.
    pub flow: Vec<FlowItem>,
    /// Replaced wholesale by each PlanUpdated (rule W4). Not merged.
    pub plan: Vec<PlanEntry>,
    /// Set by TurnStarted, cleared by the MATCHING TurnFinished (rule W7).
    /// Server guarantees at most one active turn per session (Prompt while active
    /// => Reply::TurnNotActive); the fold still guards against violations.
    pub active_turn: Option<TurnId>,
    /// request_id -> tool_call bridge so PermissionResolved can annotate the card
    /// it belonged to (rules W5/W6). Populated by PermissionRequested, drained by
    /// PermissionResolved.
    pub pending_perm_tools: BTreeMap<RequestId, ToolCallId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub title: String,
    pub kind: ToolCallKind,
    pub status: ToolCallStatus,
    pub output: Option<String>,
    /// Opaque vendor extras, passed through from ToolCallUpsert._meta.
    pub _meta: Option<Value>,
    /// Stamped when the permission over this tool resolves (W6). None until then;
    /// None means "never asked", which is exactly why PermOutcome exists instead
    /// of a bare Option<OptionId>.
    pub perm: Option<PermOutcome>,
}

/// Tri-state outcome for a tool card. Chosen carries whichever option id the user
/// picked (allow AND reject variants alike — the UI renders the name from its own
/// sources); Cancelled covers dismiss, watchdog timeout and turn-cancel sweep.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PermOutcome {
    Chosen(OptionId),
    Cancelled,
}

/// usize indexes into `messages` — stable because messages is append-only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FlowItem {
    Message(usize),
    Tool(ToolCallId),
}

/// Get-or-create the thread for `session`, logging when the creation is IMPLICIT
/// (anything other than rule W0's SessionCreated — implies replay started mid-session
/// or a snapshot cut).
fn ensure<'a>(
    threads: &'a mut BTreeMap<SessionId, ThreadState>,
    session: &SessionId,
    implicit: bool,
) -> &'a mut ThreadState {
    if implicit && !threads.contains_key(session) {
        tracing::debug!(
            session = %session,
            "auto-vivified thread outside of SessionCreated"
        );
    }
    threads.entry(session.clone()).or_default()
}

/// Fold rules — the ONLY writer of ThreadsState on either side.
///
///   W0  SessionCreated: ensure(false); overwrite cwd + mcp_servers. Nothing else —
///       agent linkage lives in AgentsState, never duplicated here.
///   W1  TurnStarted: ensure; set active_turn. Overwriting a DIFFERENT Some => warn!
///       but proceed last-writer-wins.
///   W2  Chunk: ensure; MERGE against flow.last() ONLY — same-role message index
///       continues its text; otherwise push new Message + flow item. Never search
///       backwards past a tool card. `turn` accepted, NOT stored (transcript is
///       continuous across turns). NOT re-apply-safe; exactly-once delivery covers it.
///   W3  ToolCallUpsert: ensure; replace all five payload fields wholesale EXCEPT
///       `perm`, which is preserved from an existing entry. First appearance ALSO
///       appends the flow item at that position; later upserts never touch flow.
///   W4  PlanUpdated: ensure; plan replaced wholesale (ACP full-plan semantics).
///   W5  PermissionRequested: ensure; insert id bridge. Dup request_id => overwrite +
///       debug log. Options NOT copied here (perms.rs owns them).
///   W6  PermissionResolved: NO ensure — NEVER creates a thread. The event carries no
///       session id, so scan threads for who holds the request_id bridge (request ids
///       are globally unique); absent everywhere => debug log. Found but card never
///       upserted => drop silently (debug log), inventing one is out of scope.
///   W7  TurnFinished: ensure; matching turn clears active_turn; mismatched/double
///       => warn! + no-op (idempotent absorb). stop_reason not stored in v0 (no UI
///       consumer yet; revisit at M4 checkpoints).
///   W8  AgentStatus: ignore (agents.rs owns lifecycle entirely).
///
/// For cancel flows the server emits PermissionResolved { chosen: None } per swept
/// request plus TurnFinished { stop_reason: Cancelled }; perms.rs and this fold each
/// react independently and never read each other's state.
pub fn apply_thread(state: &mut ThreadsState, ev: &FxEvent) {
    match ev {
        FxEvent::SessionCreated {
            session,
            cwd,
            mcp_servers,
            ..
        } => {
            let ts = ensure(&mut state.threads, session, false);
            ts.cwd = cwd.clone();
            ts.mcp_servers = mcp_servers.clone();
        }
        FxEvent::TurnStarted { session, turn } => {
            let ts = ensure(&mut state.threads, session, true);
            if let Some(existing) = &ts.active_turn
                && existing != turn
            {
                tracing::warn!(
                    session = %session,
                    existing = %existing,
                    incoming = %turn,
                    "TurnStarted overwrote an active turn (protocol violation)"
                );
            }
            ts.active_turn = Some(turn.clone());
        }
        FxEvent::Chunk {
            session,
            role,
            text,
            ..
        } => {
            let ts = ensure(&mut state.threads, session, true);
            let mergeable = matches!(&ts.flow.last(), Some(FlowItem::Message(i)) if ts.messages[*i].role == *role);
            if mergeable {
                if let Some(FlowItem::Message(i)) = ts.flow.last() {
                    ts.messages[*i].text.push_str(text);
                }
            } else {
                ts.messages.push(Message {
                    role: *role,
                    text: text.clone(),
                });
                ts.flow.push(FlowItem::Message(ts.messages.len() - 1));
            }
        }
        FxEvent::ToolCallUpsert {
            session,
            tool_call,
            title,
            kind,
            status,
            output,
            _meta,
        } => {
            let ts = ensure(&mut state.threads, session, true);
            use std::collections::btree_map::Entry;
            match ts.tool_calls.entry(tool_call.clone()) {
                Entry::Vacant(vacant) => {
                    vacant.insert(ToolCall {
                        title: title.clone(),
                        kind: *kind,
                        status: *status,
                        output: output.clone(),
                        _meta: _meta.clone(),
                        perm: None,
                    });
                    ts.flow.push(FlowItem::Tool(tool_call.clone()));
                }
                // Wholesale replace except perm (W3).
                Entry::Occupied(mut occupied) => {
                    let prev = occupied.get_mut();
                    prev.title = title.clone();
                    prev.kind = *kind;
                    prev.status = *status;
                    prev.output = output.clone();
                    prev._meta = _meta.clone();
                }
            }
        }
        FxEvent::PlanUpdated { session, entries } => {
            let ts = ensure(&mut state.threads, session, true);
            ts.plan = entries.clone();
        }
        FxEvent::PermissionRequested {
            request_id,
            session,
            tool_call,
            ..
        } => {
            let ts = ensure(&mut state.threads, session, true);
            if ts
                .pending_perm_tools
                .insert(request_id.clone(), tool_call.tool_call.clone())
                .is_some()
            {
                tracing::debug!(
                    request = %request_id,
                    "duplicate PermissionRequested; bridged tool_call overwritten"
                );
            }
        }
        FxEvent::PermissionResolved { request_id, chosen } => {
            // W6: deliberately no ensure() — resolution never fabricates threads.
            let mut holder: Option<(SessionId, ToolCallId)> = None;
            for (session, ts) in state.threads.iter_mut() {
                if let Some(tool_call) = ts.pending_perm_tools.remove(request_id) {
                    holder = Some((session.clone(), tool_call));
                    break;
                }
            }
            match holder {
                Some((session, tool_call)) => match state
                    .threads
                    .get_mut(&session)
                    .and_then(|ts| ts.tool_calls.get_mut(&tool_call))
                {
                    Some(card) => {
                        card.perm = Some(match chosen {
                            Some(option_id) => PermOutcome::Chosen(option_id.clone()),
                            None => PermOutcome::Cancelled,
                        });
                    }
                    None => tracing::debug!(
                        request = %request_id,
                        tool_call = %tool_call,
                        "PermissionResolved named an un-upserted tool; dropping badge"
                    ),
                },
                None => tracing::debug!(
                    request = %request_id,
                    "PermissionResolved for unknown request_id; thread state untouched"
                ),
            }
        }
        FxEvent::TurnFinished { session, turn, .. } => {
            let ts = ensure(&mut state.threads, session, true);
            if ts.active_turn.as_ref() == Some(turn) {
                ts.active_turn = None;
            } else {
                tracing::warn!(
                    session = %session,
                    turn = %turn,
                    active = ?ts.active_turn,
                    "TurnFinished did not match an active turn (double/stale finish absorbed)"
                );
            }
        }
        FxEvent::AgentStatus { .. } => {} // W8
    }
}
