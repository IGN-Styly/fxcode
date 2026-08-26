//! Permission request bookkeeping: parking ACP requests until the human answers.

use std::collections::BTreeMap;

use tracing::warn;

use super::Ctx;
use crate::driver::acp::{ParkedPerm, PermRegTx};
use fxproto::event::FxEvent;
use fxproto::ids::{OptionId, RequestId, SessionId};
use fxproto::reply::{FxError, FxErrorCode, Reply};

/// Handler-layer result: infra failures only (wire outcomes are data).
pub type Result<T> = std::result::Result<T, crate::Error>;

/// One parked entry: everything needed to answer + audit a permission ask.
/// (Our SessionId rides HERE — the ACP layer only knows the raw string; the
/// translation happened at park time inside driver/acp.)
#[derive(Debug)]
pub struct PermsEntry {
    pub parked: ParkedPerm,
}

/// Runtime authority for "is this pending?". projections.perms.pending mirrors
/// it for replays/UI but respond() never consults it — races resolve by map
/// removal alone (respond step 1).
#[derive(Debug, Default)]
pub struct PendingPerms {
    /// BTreeMap on purpose: uuid-v7 RequestIds iterate oldest-first, so sweeps
    /// resolve in ASK order — deterministic assertions and a fair modal queue.
    pub(crate) map: BTreeMap<RequestId, PermsEntry>,
}

impl PendingPerms {
    /// Registration path from connection actors: the orchestrator actor loop
    /// calls this on every PermRegTx delivery, BEFORE the corresponding event
    /// becomes visible on the pump (ParkedPerm ordering contract).
    pub fn insert_parked(&mut self, parked: ParkedPerm) {
        let id = parked.core.our_id.clone();
        self.map.insert(id, PermsEntry { parked });
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Channel handed into `AcpConnection::start*`; parks are consumed by the
/// orchestrator actor and funnelled into [`PendingPerms::insert_parked`].
pub fn new_permreg_pair() -> (PermRegTx, tokio::sync::mpsc::UnboundedReceiver<ParkedPerm>) {
    tokio::sync::mpsc::unbounded_channel()
}

/// PermissionResponse → Reply::PermissionRecorded | Error(PermissionNotFound | Internal)
///
/// 1. Removal IS the resolution mark — one check covers unknown AND
///    already-resolved AND swept (reply.rs pins "no expiry timestamps here").
/// 2. The agent is answered FIRST: recorded audit rows describe reality
///    already-told-to-agent, and this unblocks the waiting turn fastest.
///    Send failure ⇒ emit PermissionResolved{chosen: None} honestly; entry was
///    already removed at step 1 ⇒ no sweep can double-emit.
/// 3. Emit PermissionResolved{chosen: Some}.
pub async fn respond(
    ctx: &mut Ctx<'_>,
    request_id: RequestId,
    option_id: OptionId,
) -> Result<Reply> {
    let entry = { ctx.pending_perms.lock().await.map.remove(&request_id) };
    let Some(entry) = entry else {
        return Ok(reply_err(
            FxErrorCode::PermissionNotFound,
            "no such pending permission",
        ));
    };

    if let Err(err) = entry.parked.respond_selected(&option_id) {
        warn!(target: "perms", request = %request_id, error = %err,
              "agent connection lost before answer landed");
        ctx.sink
            .emit(FxEvent::PermissionResolved {
                request_id: request_id.clone(),
                chosen: None,
            })
            .await?;
        return Ok(reply_err(
            FxErrorCode::Internal,
            "agent connection lost before answer landed",
        ));
    }

    ctx.sink
        .emit(FxEvent::PermissionResolved {
            request_id,
            chosen: Some(option_id),
        })
        .await?;
    Ok(Reply::PermissionRecorded)
}

/// User cancel / watchdog sweep for one session. Zero pending ⇒ zero events:
/// "cancel with nothing open" is not an event-worthy fact (TurnFinished
/// carries that story). Ask order guaranteed by BTreeMap iteration.
pub async fn sweep_cancelled(ctx: &mut Ctx<'_>, session: &SessionId) {
    // Collect ONLY ids first; removal below pops owned ParkedPerms.
    let ids: Vec<RequestId> = {
        let guard = ctx.pending_perms.lock().await;
        guard
            .map
            .iter()
            .filter(|(_, e)| &e.parked.session == session)
            .map(|(id, _)| id.clone())
            .collect()
    };

    for id in ids {
        let entry = ctx.pending_perms.lock().await.map.remove(&id);
        if let Some(entry) = entry {
            // Agent answered FIRST (see respond step 2), then our own event.
            if let Err(err) = entry.parked.respond_cancelled() {
                tracing::debug!(target: "perms", request=%id, error=%err, "responder send failed during sweep");
            }
            if let Err(err) = ctx
                .sink
                .emit(FxEvent::PermissionResolved {
                    request_id: id.clone(),
                    chosen: None,
                })
                .await
            {
                // Store failures mid-sweep are load-bearing errors: surface but
                // do not block answering remaining responders.
                tracing::error!(target: "perms", request=%id, error=?err, "sweep event append failed");
            }
        }
    }
}

/// Conn-death twin of sweep_cancelled, callable WITHOUT Ctx from turn tasks
/// (see cmd/session.rs step 8b): same order — responder answered Outcome::
/// Cancelled first, then one PermissionResolved{chosen: None} each, removing
/// from the runtime map as we go. Projections follow via the normal pipeline.
pub async fn sweep_cancelled_for_conn_death(
    perms: &mut PendingPerms,
    sink: &crate::cmd::EventSink,
    session: &SessionId,
) {
    let ids: Vec<RequestId> = perms
        .map
        .iter()
        .filter(|(_, e)| &e.parked.session == session)
        .map(|(id, _)| id.clone())
        .collect();

    for id in ids {
        if let Some(entry) = perms.map.remove(&id) {
            let _ = entry.parked.respond_cancelled();
            if let Err(err) = sink
                .emit(FxEvent::PermissionResolved {
                    request_id: id.clone(),
                    chosen: None,
                })
                .await
            {
                tracing::error!(target: "perms", request=%id, error=?err, "conn-death sweep append failed");
            }
        }
    }
}

pub(super) fn reply_err(code: FxErrorCode, message: impl Into<String>) -> Reply {
    Reply::Error(FxError {
        code,
        message: message.into(),
    })
}
