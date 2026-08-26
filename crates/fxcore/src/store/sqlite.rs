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
//! Storage representation (settled): the `json` column carries the INNER
//! `FxEvent` only, never the `Sequenced` wrapper; `seq` is reconstructed from
//! AUTOINCREMENT rowid on every read (column wins over blob). `ts`/`kind` are
//! indexing aids written but never read back — deliberate redundancy with what
//! could be parsed out of `json`, kept because human log inspection happens
//! through raw SQL more often than through code. If a ts/kind consumer shows
//! up, add an index; do not start trusting them as source of truth.
//!
//! Migration stance v0: rely solely on idempotent `CREATE TABLE IF NOT EXISTS`,
//! and stamp `PRAGMA user_version = 1` once at open. Never READ user_version yet:
//! its job is to guarantee a foothold exists so the first future schema change
//! starts migrating FROM 1 instead of discovering nobody ever wrote it. Table
//! existing with any shape ⇒ assumed compatible (only one shape has ever
//! shipped while user_version stays unread).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::Seq;
use rusqlite::{Connection, params};

use super::{EventStore, StoreError};

/// Internal page size for [`EventStore::replay`] unrolling into replay_batch.
/// Matches proj.rs's REBUILD_PAGE so both callers pay the same SQLite round-
/// trip profile (~10k rows ≈ single-digit MB JSON).
const REPLAY_PAGE: usize = 10_000;

/// Clock read helper. Unix-millis stamp for the `ts` column, taken Rust-side
/// (not SQL NOW()) — one language for clocks, testable determinism. A clock
/// behind epoch is broken-box territory; we degrade to 0 rather than panic
/// mid-write (a panic would poison the writer mutex permanently), since `ts`
/// is an informational aid, never read back.
fn unix_millis_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Encode once, use twice: returns `(kind, json)` where `kind` is the serde
/// tag ("type" field, e.g. "agent_status", "chunk") derived by serializing —
/// no hand-maintained match that can drift from fxproto's `#[serde(tag)]`.
/// A missing tag is impossible while FxEvent stays internally tagged; we fall
/// back to "" instead of a custom error because `kind` is an aid column and
/// serde_json offers no Error::custom without importing the serde trait.
fn encode(ev: &FxEvent) -> Result<(String, String), StoreError> {
    let value = serde_json::to_value(ev)?;
    let kind = value
        .get("type")
        .and_then(|tag| tag.as_str())
        .unwrap_or_default()
        .to_owned();
    Ok((kind, value.to_string()))
}

/// INSERT + rowid readback under ONE lock scope — atomic wrt other appends by
/// construction, which is exactly where the "seq strictly increasing, gapless"
/// contract lives. json column holds the INNER event; seq comes from rowid.
fn insert_event(conn: &Connection, ev: FxEvent) -> Result<Sequenced<FxEvent>, StoreError> {
    let (kind, json) = encode(&ev)?;
    let ts = unix_millis_now();
    conn.execute(
        "INSERT INTO events(ts, kind, json) VALUES(?1, ?2, ?3)",
        params![ts, kind, json],
    )?;
    // Same lock scope as the INSERT (see above): last_insert_rowid is valid
    // for this connection and cannot be raced between the two calls here.
    let seq = conn.last_insert_rowid();
    Ok(Sequenced {
        seq: Seq::new(seq as u64),
        inner: ev,
    })
}

/// Shared page-read primitive behind replay/replay_batch: COLUMN seq is
/// authoritative, blob never carried one. Ascending, strictly-after `after`.
fn read_batch(
    conn: &Connection,
    after: u64,
    limit: usize,
) -> Result<Vec<Sequenced<FxEvent>>, StoreError> {
    let mut stmt = conn
        .prepare_cached("SELECT seq, json FROM events WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2")?;
    let rows = stmt.query_map(params![after as i64, limit as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (seq, json) = row?;
        let inner: FxEvent = serde_json::from_str(&json)?;
        out.push(Sequenced {
            seq: Seq::new(seq as u64),
            inner,
        });
    }
    Ok(out)
}

pub struct SqliteStore {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

// Drop semantics: plain Connection drop closes + auto-checkpoints WAL when we
// hold the last handle (SQLite default). No explicit Drop impl needed unless a
// test proves otherwise; reopen_persists... below is the guard.
impl SqliteStore {
    /// Open-or-create. Steps IN ORDER:
    ///   1. create parent dirs if missing (self-contained; config already made
    ///      data_dir but defensive re-mkdir is free)
    ///   2. Connection::open(path)            (":memory:" passes through ⇒
    ///      in-memory db, what unit tests may use instead of tempdirs)
    ///   3. PRAGMA journal_mode=WAL
    ///   4. PRAGMA synchronous=NORMAL         (SQLite's recommended WAL pairing:
    ///      app crashes lose nothing; OS crash/power-loss may lose the last
    ///      transactions — accepted v0 tradeoff, revisit only with a real
    ///      durability requirement)
    ///   5. PRAGMA busy_timeout=5000          (ms; belt-and-braces vs external
    ///      inspection tools — our access pattern can't self-contend under
    ///      Mutex + sink serialization)
    ///   6. schema block from header (CREATE IF NOT EXISTS + user_version=1)
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path: &Path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            // StoreError has no Io variant (pinned variant list): mkdir
            // failure is logged and deferred to Connection::open, which
            // surfaces the real cause as Sqlite(CantOpen).
            if std::fs::create_dir_all(parent).is_err() {
                tracing::warn!(
                    parent = %parent.display(),
                    "could not pre-create store dir; letting sqlite open report the failure"
                );
            }
        }
        let conn = Self::open_connection(path)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Trivial wrapper used by Orchestrator::new step 1:
    /// `Arc<dyn EventStore>` straight out of a path.
    pub fn open_shared(path: impl AsRef<Path>) -> Result<Arc<dyn EventStore>, StoreError> {
        Ok(Arc::new(Self::open(path)?) as Arc<dyn EventStore>)
    }

    fn open_connection(path: &Path) -> Result<Connection, StoreError> {
        let conn = Connection::open(path)?;
        // journal_mode=WAL both sets and reports; on ":memory:" SQLite replies
        // "memory" (in-memory dbs cannot be WAL), which we accept silently —
        // the file-backed invariant is pinned by journal_mode_is_wal below.
        let _reported: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.execute_batch(
            "PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                 seq  INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts   INTEGER NOT NULL,
                 kind TEXT NOT NULL,
                 json TEXT NOT NULL
             );
             PRAGMA user_version = 1;",
        )?;
        Ok(conn)
    }

    /// Shared boilerplate for all four ops: clone the Arc, hop onto the blocking
    /// pool, lock, run the op, map Join errors deliberately (never unwrap — see
    /// mod.rs Join variant note). Guard is dropped before any await point.
    async fn run<T, F>(&self, op: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().map_err(|_| StoreError::Poisoned)?;
            op(&conn)
        })
        .await
        .map_err(StoreError::from)?
    }
}

impl EventStore for SqliteStore {
    fn append(&self, ev: FxEvent) -> BoxFuture<'_, Result<Sequenced<FxEvent>, StoreError>> {
        Box::pin(self.run(move |conn| insert_event(conn, ev)))
    }

    // Unbounded contract honored via internal batch loop: collect pages until
    // a short page arrives. Caller must already know the tail is small.
    fn replay(&self, after: Seq) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StoreError>> {
        Box::pin(async move {
            let mut out = Vec::new();
            let mut cursor = after.as_u64();
            loop {
                let batch = self
                    .run(move |conn| read_batch(conn, cursor, REPLAY_PAGE))
                    .await?;
                let done = batch.len() < REPLAY_PAGE;
                cursor = batch.last().map_or(cursor, |ev| ev.seq.as_u64());
                out.extend(batch);
                if done {
                    break;
                }
            }
            Ok(out)
        })
    }

    fn replay_batch(
        &self,
        after: Seq,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StoreError>> {
        Box::pin(self.run(move |conn| read_batch(conn, after.as_u64(), limit)))
    }

    fn head_seq(&self) -> BoxFuture<'_, Result<Seq, StoreError>> {
        Box::pin(self.run(|conn| {
            let max: i64 =
                conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                    row.get(0)
                })?;
            Ok(Seq::new(max as u64))
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;
    use std::path::PathBuf;

    use fxproto::content::{Role, StopReason, ToolCallKind, ToolCallStatus};
    use fxproto::driver::DriverId;
    use fxproto::event::AgentStatus;
    use fxproto::ids::{AgentId, SessionId, ToolCallId, TurnId};
    use serde_json::json;

    use super::*;

    /// Hermetic scratch dir WITHOUT the tempfile crate: std::env::temp_dir() +
    /// pid/nanos naming, removed by Drop (best-effort, same as tempfile does).
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("fxcore-test-{tag}-{}-{nanos}", std::process::id(),));
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self(dir)
        }
    }

    impl Deref for Scratch {
        type Target = PathBuf;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Fixture spread across several variants — includes `_meta: Option<Value>`
    /// with nested arrays/nulls (JSON round-trip sentinel) and PathBuf cwd.
    fn sample_events(prefix: &str) -> Vec<FxEvent> {
        let session = || SessionId::from_raw(format!("{prefix}-s"));
        let turn = || TurnId::from_raw(format!("{prefix}-t"));
        vec![
            FxEvent::AgentStatus {
                agent: AgentId::from_raw(format!("{prefix}-a")),
                driver: DriverId::ClaudeCode,
                status: AgentStatus::Starting,
            },
            FxEvent::SessionCreated {
                session: session(),
                agent: AgentId::from_raw(format!("{prefix}-a")),
                cwd: "/tmp/demo".into(),
                mcp_servers: vec![],
            },
            FxEvent::Chunk {
                session: session(),
                turn: turn(),
                role: Role::Agent,
                text: format!("{prefix} hello"),
            },
            FxEvent::ToolCallUpsert {
                session: session(),
                tool_call: ToolCallId::from_raw(format!("{prefix}-tc")),
                title: "ls".into(),
                kind: ToolCallKind::Execute,
                status: ToolCallStatus::Completed,
                output: Some("file.txt".into()),
                _meta: Some(json!({"vendor": {"deep": [1, true, null]}, "n": 42})),
            },
            FxEvent::TurnFinished {
                session: session(),
                turn: turn(),
                stop_reason: StopReason::EndTurn,
            },
            FxEvent::AgentStatus {
                agent: AgentId::from_raw(format!("{prefix}-a")),
                driver: DriverId::ClaudeCode,
                status: AgentStatus::Crashed {
                    exit_code: Some(-9),
                },
            },
        ]
    }

    fn chunk_ev(n: usize) -> FxEvent {
        FxEvent::Chunk {
            session: SessionId::from_raw("store-s".into()),
            turn: TurnId::from_raw("store-t".into()),
            role: Role::Agent,
            text: format!("chunk-{n}"),
        }
    }

    fn assert_strictly_ascending(events: &[Sequenced<FxEvent>], expect_first: u64) {
        for (i, ev) in events.iter().enumerate() {
            assert_eq!(
                ev.seq.as_u64(),
                expect_first + i as u64,
                "gap/order break at {i}"
            );
        }
    }

    #[tokio::test]
    async fn fresh_head_seq_is_zero_and_memory_path_works() {
        let store = SqliteStore::open(":memory:").unwrap();
        assert_eq!(store.head_seq().await.unwrap().as_u64(), 0);
        assert!(store.replay(Seq::new(0)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn appends_replay_gapless_ascending_head_tracks_last() {
        let store = SqliteStore::open(":memory:").unwrap();
        const N: u64 = 25;
        for i in 1..=N {
            let stamped = store.append(chunk_ev(i as usize)).await.unwrap();
            // Append returns the freshly-stamped event itself: 1..N, gapless.
            // (FxEvent has no PartialEq by design → byte-compare the JSON.)
            assert_eq!(stamped.seq.as_u64(), i);
            assert_eq!(
                serde_json::to_string(&stamped.inner).unwrap(),
                serde_json::to_string(&chunk_ev(i as usize)).unwrap()
            );
        }
        assert_eq!(store.head_seq().await.unwrap().as_u64(), N);
        let all = store.replay(Seq::new(0)).await.unwrap();
        assert_eq!(all.len(), N as usize);
        assert_strictly_ascending(&all, 1);
    }

    #[tokio::test]
    async fn suffix_replay_returns_only_strictly_after_cursor() {
        let store = SqliteStore::open(":memory:").unwrap();
        for i in 1..=10 {
            store.append(chunk_ev(i)).await.unwrap();
        }
        let suffix = store.replay(Seq::new(4)).await.unwrap();
        assert_eq!(
            suffix.iter().map(|e| e.seq.as_u64()).collect::<Vec<_>>(),
            vec![5, 6, 7, 8, 9, 10]
        );
        // Cursor == head ⇒ empty (contract: empty ONLY there).
        assert!(
            store
                .replay(store.head_seq().await.unwrap())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn replay_batch_pages_exact_then_short_means_done() {
        let store = SqliteStore::open(":memory:").unwrap();
        for i in 1..=7 {
            store.append(chunk_ev(i)).await.unwrap();
        }
        let p1 = store.replay_batch(Seq::new(0), 3).await.unwrap();
        assert_eq!(
            p1.iter().map(|e| e.seq.as_u64()).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        let p2 = store.replay_batch(p1.last().unwrap().seq, 3).await.unwrap();
        assert_eq!(
            p2.iter().map(|e| e.seq.as_u64()).collect::<Vec<_>>(),
            [4, 5, 6]
        );
        // Short page == end of log signal (proj.rs rebuild loop driver).
        let p3 = store.replay_batch(p2.last().unwrap().seq, 3).await.unwrap();
        assert_eq!(p3.iter().map(|e| e.seq.as_u64()).collect::<Vec<_>>(), [7]);
        let head = store.head_seq().await.unwrap();
        assert!(store.replay_batch(head, 3).await.unwrap().is_empty());
        // limit > remaining ⇒ everything at once.
        assert_eq!(store.replay_batch(Seq::new(0), 100).await.unwrap().len(), 7);
        // Growth past a drained cursor keeps the stream going.
        store.append(chunk_ev(8)).await.unwrap();
        let next = store.replay_batch(p3.last().unwrap().seq, 3).await.unwrap();
        assert_eq!(next.iter().map(|e| e.seq.as_u64()).collect::<Vec<_>>(), [8]);
    }

    #[tokio::test]
    async fn reopen_preserves_head_seq_and_exact_json_bytes() {
        let scratch = Scratch::new("reopen");
        let db = scratch.join("events.db");

        let originals: Vec<Sequenced<FxEvent>> = {
            let store = SqliteStore::open(&db).unwrap();
            let mut stamped = Vec::new();
            for ev in sample_events("reopen") {
                stamped.push(store.append(ev).await.unwrap());
            }
            stamped
        }; // store dropped → conn closed → WAL checkpointed
        assert!(!originals.is_empty());

        let store = SqliteStore::open(&db).unwrap();
        assert_eq!(
            store.head_seq().await.unwrap().as_u64(),
            originals.last().unwrap().seq.as_u64()
        );
        let replayed = store.replay(Seq::new(0)).await.unwrap();
        assert_eq!(replayed.len(), originals.len());
        for (orig, back) in originals.iter().zip(replayed.iter()) {
            // Byte-compare full Sequenced JSON (seq transparent u64): proves
            // lossless round-trip incl. nested serde_json::Value `_meta`.
            let a = serde_json::to_string(orig).unwrap();
            let b = serde_json::to_string(back).unwrap();
            assert_eq!(a, b);
        }
        // Schema-version foothold exists (never read in production paths yet).
        let guard = store.conn.lock().unwrap();
        let version: i64 = guard
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1);
    }

    #[tokio::test]
    async fn journal_mode_is_wal_on_disk_store() {
        let scratch = Scratch::new("wal");
        let db = scratch.join("events.db");
        let store = SqliteStore::open(&db).unwrap();
        drop(
            store
                .append(FxEvent::AgentStatus {
                    agent: AgentId::from_raw("a".into()),
                    driver: DriverId::GeminiCli,
                    status: AgentStatus::Ready,
                })
                .await
                .unwrap(),
        );
        // Independent connection: WAL persists in the db header across handles.
        let probe = Connection::open(&db).unwrap();
        let mode: String = probe
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[tokio::test]
    async fn kind_column_holds_serde_tags_in_append_order() {
        let store = SqliteStore::open(":memory:").unwrap();
        let events = sample_events("kind"); // first three: agent_status, session_created, chunk
        for ev in &events[..3] {
            store.append(ev.clone()).await.unwrap();
        }
        let guard = store.conn.lock().unwrap();
        let kinds: Vec<String> = guard
            .prepare("SELECT kind FROM events ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(kinds, ["agent_status", "session_created", "chunk"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_stay_totally_ordered_gapless() {
        let store = Arc::new(SqliteStore::open(":memory:").unwrap());
        let tasks: Vec<_> = (0..4usize)
            .map(|w| {
                let store = Arc::clone(&store);
                tokio::spawn(async move {
                    for i in 0..5usize {
                        store
                            .append(FxEvent::Chunk {
                                session: SessionId::from_raw(format!("worker-{w}-s")),
                                turn: TurnId::from_raw(format!("worker-{w}-t")),
                                role: Role::Agent,
                                text: format!("w{w}-{i}"),
                            })
                            .await
                            .unwrap();
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        let all = store.replay(Seq::new(0)).await.unwrap();
        assert_eq!(all.len(), 20);
        assert_strictly_ascending(&all, 1);
        // Interleave order among workers is impl-chosen; every payload present
        // exactly once proves no lost/duplicated rows.
        let mut texts: Vec<String> = all
            .iter()
            .filter_map(|e| match &e.inner {
                FxEvent::Chunk { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        texts.sort();
        let want: Vec<String> = (0..4usize)
            .flat_map(|w| (0..5usize).map(move |i| format!("w{w}-{i}")))
            .collect();
        assert_eq!(texts, want);
    }
}
