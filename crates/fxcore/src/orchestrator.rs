//! Orchestrator — THE fxcore entrypoint. Everything else serves this.

// Imports to restore as you define the types:
// use fxproto::command::Command;
// use fxproto::event::Sequenced;
// use fxproto::reply::Reply;
//
// use crate::config::Config;

// TODO:
//
// pub struct Orchestrator {
//     // inside a single struct, shared as Arc<Orchestrator> by fxserver:
//     //
//     // cmd_tx: mpsc::Sender<Job>,          // all mutations queue here
//     // store: Arc<dyn EventStore>,         // append-only log
//     // registry: DriverRegistry,           // driver specs + detection cache
//     // conns: DashMap-ish of AgentId → running AcpConnection handle
//     // projections: RwLock<model states>,  // rebuilt at boot, updated post-append
//     // pending_perms: perms bookkeeping    // RequestId → oneshot to ACP actor
// }
//
// impl Orchestrator {
//     /// Opens the store, replays the log into projections, spawns the actor task.
//     pub async fn new(cfg: Config) -> Result<Self>;
//
//     /// Queue a command; returns when its handler completes. Exactly one Reply.
//     pub async fn execute(&self, cmd: Command) -> Result<Reply>;
//
//     /// Post-persist fanout. Subscribers (one per ws client) get Sequenced<FxEvent>s.
//     pub fn subscribe(&self) -> broadcast::Receiver<Sequenced<FxEvent>>; // wrap in bus type
// }
//
// Actor sketch (implement in this file or cmd/mod.rs):
//   loop { recv Job { cmd, reply_tx } → dispatch via cmd::handle(ctx, cmd) }
//   where ctx bundles &store, &registry, &conns, &projections, event_sink.
//   Handlers may spawn turn tasks; those tasks emit through event_sink which does
//   store.append(seq assigned) → projections update → bus.send.
//
// TODO: shutdown — drop/close cmd channel, drain, signal AcpConnections to kill children
// gracefully (SIGTERM then SIGKILL timeout). fxserver calls this on SIGTERM/Ctrl-C.
