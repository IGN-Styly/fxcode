//! SQLite implementation of EventStore. Single file, WAL mode, single writer.
//!
//! Concurrency: OPTION A (finalized) — `Mutex<rusqlite::Connection>` +
//! `tokio::task::spawn_blocking` per op. Option B (dedicated writer task owning
//! the conn via mpsc) is the documented upgrade path, NOT v0.
//!
//! Rationale for A: (1) write volume is per-agent-turn events — tens of rows/
//! second worst case, appends are single-row INSERTs; (2) the strict ordering
//! story does NOT live here — EventSink's mutex upstream (cmd/mod.rs) already
//! serializes every caller, so B's "cleaner ordering" buys nothing real; (3) A
//! keeps the store stateless-simple: no task lifecycle to manage in shutdown,
//! no channel to drain. Switch trigger: if `tracing` spans show append latency
//! breaching ~5 ms p99 under load, move to B behind this SAME trait — trait
//! objects make it invisible to all callers.

//! Schema (v0 — migration stance below; no migration framework yet):
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS events (
//!   seq  INTEGER PRIMARY KEY AUTOINCREMENT,
//!   ts   INTEGER NOT NULL,          -- unix millis, stamped by Rust at insert
//!   kind TEXT    NOT NULL,          -- FxEvent serde tag; debugging queries only
//!   json TEXT    NOT NULL           -- serialized INNER FxEvent (NOT Sequenced;
//!                                   --   seq lives in the column and wins on read)
//! );
//! PRAGMA user_version = 1;          -- stamped at open; see migration stance
//! ```
//!
//! Index stance: NO secondary indexes in v0. Every product query is seq-keyed
//! (`WHERE seq > ? ORDER BY seq`, `MAX(seq)`) and satisfied by the PK index.
//! `kind` exists only for ad-hoc debugging (`GROUP BY kind` full scans are fine
//! at human log sizes). Trigger to revisit: a REAL query that filters by ts or
//! kind gets `CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind)` then.
//!
//! Migration stance v0: rely solely on idempotent `CREATE TABLE IF NOT EXISTS`,
//! and stamp `PRAGMA user_version = 1` once at open. Never READ user_version yet:
//! its job is to guarantee a foothold exists so the first future schema change
//! starts migrating FROM 1 instead of discovering nobody ever wrote it. Table
//! existing with any shape ⇒ assumed compatible (only one shape has ever
//! shipped while user_version stays unread).

// TODO:
//
// pub struct SqliteStore { conn: Arc<Mutex<rusqlite::Connection>> }
//
// impl SqliteStore {
//     /// Open-or-create. Steps IN ORDER:
//     ///   1. create parent dirs if missing (self-contained; config already made
//     ///      data_dir but defensive re-mkdir is free)
//     ///   2. Connection::open(path)            (":memory:" passes through ⇒
//     ///      in-memory db, what unit tests may use instead of tempdirs)
//     ///   3. PRAGMA journal_mode=WAL
//     ///   4. PRAGMA synchronous=NORMAL         (SQLite's recommended WAL pairing:
//     ///      app crashes lose nothing; OS crash/power-loss may lose the last
//     ///      transactions — accepted v0 tradeoff, revisit only with a real
//     ///      durability requirement)
//     ///   5. PRAGMA busy_timeout=5000          (ms; belt-and-braces vs external
//     ///      inspection tools — our access pattern can't self-contend under
//     ///      Mutex + sink serialization)
//     ///   6. schema block from header (CREATE IF NOT EXISTS + user_version=1)
//     pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError>;
//
//     /// Trivial wrapper used by Orchestrator::new step 1:
//     /// `Arc::new(Self::open(...)?) as Arc<dyn EventStore>`
// }
//
// impl EventStore for SqliteStore {
//     // shared boilerplate for all four ops:
//     //   let conn = self.conn.clone();
//     //   tokio::task::spawn_blocking(move || { let c = conn.lock().map_err(|_| StoreError::Poisoned)?; ... }).await?
//     //   (JoinError from an aborted runtime → map into StoreError::Sqlite(...
//     //    "join") or add Join variant; pick when writing, don't unwrap.)
//     //
//     // append(ev):
//     //   kind = serde tag via the same mechanism fxproto exposes for tags — if
//     //          none is public, derive it as `"type"` field of
//     //          serde_json::to_value(&ev)?["type"].as_str() (single helper fn)
//     //   ts   = SystemTime::now() → unix millis (Rust side, not SQL NOW(): one
//     //          language for clocks, testable determinism)
//     //   json = serde_json::to_string(&ev)?        ← INNER event only; NO seq
//     //   INSERT INTO events(ts, kind, json) VALUES(?1, ?2, ?3)
//     //   seq  = c.last_insert_rowid()   (SAME lock scope as the INSERT — under
//     //          Mutex this is atomic wrt other appends by construction)
//     //   return Ok(Sequenced { seq: Seq(seq as u64), inner: ev })
//     //
//     // replay(after): loop replay_batch internally: collect pages until a short
//     //   page arrives; assemble Vec. (Documented-unbounded contract honored.)
//     //
//     // replay_batch(after, limit):
//     //   SELECT seq, json FROM events WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2
//     //   for each row: inner: FxEvent = serde_json::from_str(json)?;
//     //                 push Sequenced { seq: Seq(row.seq as u64), inner }   ←
//     //                 COLUMN seq is authoritative; blob never carried one.
//     //
//     // head_seq():
//     //   SELECT COALESCE(MAX(seq), 0) FROM events
// }
//
// // Drop semantics: plain Connection drop closes + auto-checkpoints WAL when we
// // hold the last handle (SQLite default). No explicit Drop impl needed unless a
// // test proves otherwise; reopen-persistence test below is the guard.
//
#[cfg(test)]
mod tests {
    // TODO: tempdir-backed tests (impl.md Phase 2.2):
    // - append N events → replay(0) returns all in order w/ seqs 1..N, gapless
    // - replay(k) returns suffix
    // - replay_batch(after, limit): exact page sizes, short-page-means-done,
    //   empty only after head
    // - head_seq on fresh store == 0
    // - reopening the file preserves data (WAL checkpoint on close)
    // - kind column stores the serde tag correctly ("agent_status", ...)
    // - :memory: path works (lets non-tempdir unit tests share these helpers)
    use super::*;
}
