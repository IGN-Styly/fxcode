//! Permission request bookkeeping: parking ACP requests until the human answers.

// Imports to restore as you define the types:
// use super::Ctx;
// use fxproto::ids::{OptionId, RequestId, SessionId};

// TODO:
//
// pub struct PendingPerms {
//     /// RequestId → the ACP-side responder that completes the parked server→client request.
//     map: HashMap<RequestId, PendingAcpRequest>,
// }
//
// pub async fn respond(ctx: &mut Ctx, request_id: RequestId, option_id: OptionId) -> Result<Reply>;
//   validate pending (projections.perms) → forward to owning conn
//   → PermissionResolved event → Reply::PermissionRecorded
//   unknown/expired ⇒ FxError reply (client may be stale).
//
// pub fn sweep_cancelled(&mut self, session: SessionId);
//   turn was cancelled: answer ALL unanswered requests for this session with outcome
//   "cancelled" (ACP REQUIRES clients respond to pending permission requests), emit
//   PermissionResolved { chosen: None } events.
