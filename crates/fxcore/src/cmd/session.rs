//! Session/agent/turn lifecycle handlers.

// Imports to restore as you define the types:
// use std::path::PathBuf;
//
// use super::Ctx;
// use fxproto::command::Command;
// use fxproto::content::ContentBlock;
// use fxproto::driver::DriverId;
// use fxproto::ids::{AgentId, SessionId};
// use fxproto::reply::Reply;

// TODO: one fn per command branch:
//
// pub async fn start_agent(ctx: &mut Ctx, driver: DriverId) -> Result<Reply>;
//   registry.plan(driver) → AgentStatus::Starting event → spawn conn
//   → on ready emit AgentStatus::Ready → Reply::Started
//   failure path: AgentStatus::Crashed event + Error reply. Always leave a trail.
//
// pub async fn new_session(ctx: &mut Ctx, agent: AgentId, cwd: PathBuf, mcp: Vec<..>) -> Result<Reply>;
//   validate agent exists+ready → conn.new_session() → SessionCreated event
//   (carries agent + cwd + mcp_servers — the durable record that this session exists)
//   → Reply::SessionCreated. (cwd validation is the AGENT's job per ACP; we just pass it.)
//
// pub async fn prompt(ctx: &mut Ctx, s: SessionId, blocks: Vec<ContentBlock>) -> Result<Reply>;
//   validate session + no active turn → echo user Chunk events? DECISION:
//     yes — persist the user's own text so replay reconstructs full transcripts.
//   TurnStarted event → spawn turn task:
//       conn.prompt(acp_session).await  // resolves at stopReason
//       on completion: TurnFinished { stop_reason } event
//       on conn death mid-turn: TurnFinished { stop_reason: Cancelled } + Crashed agent event
//   → Reply::PromptAccepted immediately (turn streams via events).
//
// pub async fn cancel(ctx: &mut Ctx, s: SessionId) -> Result<Reply>;
//   validate active turn → conn.cancel() → perms::sweep_cancelled(s)
//   (TurnFinished arrives from the turn task when the agent acknowledges; if it never
//    does, a watchdog timeout force-finishes with Cancelled — implement watchdog here.)
