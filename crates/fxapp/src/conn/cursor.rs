//! Resume cursor: how reconnects replay instead of resyncing.

use std::path::PathBuf;

// Imports to restore as you implement:
// use std::fs::{File, OpenOptions};
// use std::io::Write;
// use std::os::unix::fs::{PermissionsExt, OpenOptionsExt};   // POSIX-only, no Windows paths
//
// use serde::{Deserialize, Serialize};

// TODO:
//
// /// Persisted to ~/.fxcode/client-state.json (also: server url + token + theme choice;
// /// theme key reserved now, owned by theme.rs later).
// #[derive(Serialize, Deserialize, Default)]
// pub struct ClientState {
//     pub server_url: Option<String>,
//     pub token: Option<String>,      // plaintext ok? yes: file is chmod 600; identical
//                                     // trust to the server's own plaintext-at-rest token
//                                     // (pair.rs "no hashing" rationale applies 1:1)
//     pub last_seq: u64,
// }
//
// SEQ EDGE RULES (locked decision: the cursor FILE stores a BARE u64 — fxproto ids::Seq is
// #[serde(transparent)] so the bytes are identical anyway; convert at these exact seams
// and nowhere else in fxapp):
//   ingest:    ev.seq.as_u64()                 → ClientState.last_seq
//   subscribe: Seq::from_raw(self.last_seq)    → envelope Message::Subscribe { .. }
//   snapshot:  snapshot.baseline_seq.as_u64()  → ClientState.last_seq
//
// pub fn load(dir: &Path) -> ClientState
//   Missing file ⇒ Default silently (first run is normal). Corrupt JSON / wrong shape ⇒
//   Default + tracing::warn!(error). NEVER attempt partial salvage: a lost cursor costs
//   one bounded replay (≤ REPLAY_GAP_LIMIT events) or one SnapshotRequired next boot.
//
// pub fn save(&self, dir: &Path) -> std::io::Result<()>      ATOMIC-WRITE RECIPE, exactly:
//   1. create_dir_all(dir); if that CREATED it, chmod 0o700 right after (pre-existing
//      dirs left alone — pair.rs precedent; create_dir_all cannot set modes).
//   2. serde_json::to_string_pretty(self) → write via OpenOptions::new()
//      .mode(0o600)                      ← mode set ON CREATION of the tmp file: there is
//                                          no instant where state sits world-readable
//      .write(true).create().truncate(true)
//      on "<dir>/client-state.json.tmp", then sync_all() before renaming (durable tmp).
//   3. std::fs::rename(tmp ⇒ client-state.json) — atomic within the directory; readers
//      see either the old or the new file whole, never a torn write.
//   4. Any step Err ⇒ best-effort remove_file(tmp), return the Err (caller logs; the
//      PREVIOUS file stays valid — save failure never corrupts boot state).
//
// TIMING RULES (relative to folds — conn/mod.rs enforces the order, this file owns why):
//   - Per live/replayed event: fold FIRST (store/mod.rs apply), THEN last_seq = ev.seq,
//     THEN save(). Crash between fold and save ⇒ next boot replays ≥ cursor; folds are
//     designed for re-delivery of keyed events and replay windows stay tiny, so erring
//     toward UNDER-cursor (save after fold, never before) always replays forward safely.
//   - NEVER save(last_seq = seq) BEFORE the fold for that seq has run: an over-cursor
//     save can permanently skip unapplied events — that is the one unrecoverable bug here.
//   - Replay bursts use the same per-event cadence (bounded by REPLAY_GAP_LIMIT work);
//     simplicity beats batching until profiling says otherwise.
//   - SnapshotRequired path: stores REPLACED first, last_seq = baseline_seq, ONE save.
//
// NOTE (alignment verified): `snapshot.baseline_seq` matches envelope.rs's sole baseline
// carrier field name — nothing legacy (`baseline`) remains anywhere.
