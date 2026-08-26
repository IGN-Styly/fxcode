//! Resume cursor: how reconnects replay instead of resyncing.

use std::path::PathBuf;

// TODO:
//
// /// Persisted to ~/.fxcode/client-state.json (also: server url + token + theme choice).
// #[derive(Serialize, Deserialize, Default)]
// pub struct ClientState {
//     pub server_url: Option<String>,
//     pub token: Option<String>,      // plaintext ok? file is chmod 600; same trust as server side
//     pub last_seq: u64,
// }
//
// pub fn load() -> ClientState;        // missing/corrupt file ⇒ Default + warn
// pub fn save(&self) -> Result<()>;    // atomic write (tmp + rename), chmod 600
//
// Flow rules (conn/mod.rs owns enforcement):
// - bump last_seq ONLY after the fold succeeded — crash between fold and save means
//   replay of a few events on next start; folds must be idempotent-safe under re-apply.
// - SnapshotRequired ⇒ reset last_seq = snapshot.baseline_seq and rebuild stores from it.
