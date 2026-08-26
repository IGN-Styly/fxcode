//! SQLite implementation of EventStore. Single file, WAL mode, single writer.
//!
//! Schema (v0 — no migration framework yet; just CREATE IF NOT EXISTS):
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS events (
//!   seq  INTEGER PRIMARY KEY AUTOINCREMENT,
//!   ts   INTEGER NOT NULL,          -- unix millis
//!   kind TEXT    NOT NULL,          -- FxEvent tag, for cheap debugging queries
//!   json TEXT    NOT NULL           -- full Sequenced<FxEvent>
//! );
//! ```

// TODO:
//
// pub struct SqliteStore { conn: std::sync::Mutex<rusqlite::Connection> /* or channel to writer task */ }
//
// Concurrency decision to make here:
//   Option A: Mutex<Connection> + tokio::task::spawn_blocking per op. Simplest; fine for v0
//             (agent traffic is slow; appends are tiny).
//   Option B: dedicated writer task owning the conn via mpsc. Slightly more code,
//             cleaner ordering story.
// Pick A first; B is the upgrade path. Document choice here once made.
//
// impl:
//   SqliteStore::open(path) → PRAGMA journal_mode=WAL; busy_timeout; foreign_keys=ON.
//   append(ev): INSERT; last_insert_rowid() becomes seq; return Sequenced{seq, ev}.
//               NOTE: seq assignment must be INSIDE the same lock/write as insert.
//   replay(after): SELECT ... WHERE seq > ? ORDER BY seq ASC; parse json back.
//   head_seq(): SELECT COALESCE(MAX(seq), 0).
//
#[cfg(test)]
mod tests {
    // TODO: tempdir-backed tests:
    // - append N events → replay(0) returns all in order w/ seqs 1..N
    // - replay(k) returns suffix
    // - head_seq on fresh store == 0
    // - reopening the file preserves data (WAL checkpoint on close)
    // - kind column stores the serde tag correctly
    use super::*;
}
