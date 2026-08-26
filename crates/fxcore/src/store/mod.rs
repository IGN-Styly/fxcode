//! Event persistence contract.
//!
//! The single source of global order: [`EventStore::append`] stamps every
//! [`Sequenced<FxEvent>`] with a fresh `Seq` and everything else in the system
//! (bus fanout, replay, cursors) consumes that order as read-only truth. See
//! fxproto/src/ids.rs WHO-MINTS-WHAT — fxcore mints NO ids here, least of all
//! `Seq`, which comes back out of SQLite's rowid counter.

pub mod sqlite;

use futures::future::BoxFuture;
use fxproto::event::{FxEvent, Sequenced};
use fxproto::ids::Seq;

/// Async-strategy decision (settled): PLAIN dyn-safe methods returning boxed
/// futures via `futures::future::BoxFuture`. No #[async_trait] (new dep for no
/// gain), no RPFFI (`Arc<dyn EventStore>` is the shared shape everywhere — see
/// cmd/mod.rs Ctx and orchestrator.rs fields). Overhead irrelevant: every impl
/// internally does spawn_blocking anyway.
///
/// NOTE on imports: the original TODO also carried `use std::future::Future`,
/// but nothing below names `Future` directly (BoxFuture already pins one), so
/// it is omitted to keep clippy's unused-imports warning at zero.
pub trait EventStore: Send + Sync {
    /// Persist and stamp seq. Contract:
    /// - seq comes from a single monotonic source in the impl (SQLite rowid);
    ///   first event ⇒ 1, strictly increasing, gapless, never reused.
    /// - Concurrent appends: totally ordered, order chosen by the impl's
    ///   internal serialization. In practice EventSink's mutex upstream already
    ///   serializes callers (cmd/mod.rs), so ordering surprises cannot surface.
    fn append(&self, ev: FxEvent) -> BoxFuture<'_, Result<Sequenced<FxEvent>, StoreError>>;

    /// All events strictly after `after`, ascending, UNBOUNDED length.
    /// Callers must already know the tail is small (handshake gap check passed).
    /// For unbounded walks use replay_batch.
    ///
    /// `after` is a cursor, not an index: Seq(0) (or head_seq of an empty log)
    /// replays everything.
    fn replay(&self, after: Seq) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StoreError>>;

    /// Pagination primitive: at most `limit` rows after `after`, ascending.
    /// Fewer than `limit` rows = end of log reached (loop driver for proj.rs
    /// rebuild: `store.replay_batch(cursor, REBUILD_PAGE).await` — and for any
    /// future streaming handoff). May return an empty Vec only when
    /// `after == head_seq`.
    fn replay_batch(
        &self,
        after: Seq,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<Sequenced<FxEvent>>, StoreError>>;

    /// Current max seq (0 for empty log). Never regresses across calls.
    fn head_seq(&self) -> BoxFuture<'_, Result<Seq, StoreError>>;
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization failure: {0}")]
    Serde(#[from] serde_json::Error),
    /// Writer mutex poisoned (a prior op panicked mid-write). Fail hard; WAL
    /// contents stay consistent but we refuse to guess from here on.
    #[error("store writer lock poisoned")]
    Poisoned,
    /// spawn_blocking task aborted/panicked at runtime shutdown (JoinError).
    /// The sqlite.rs TODO allowed either mapping it into Sqlite or adding a
    /// variant; a dedicated variant was chosen because JoinError carries
    /// structured info (was_cancelled) that lying inside a rusqlite error
    /// string would hide.
    #[error("store blocking task aborted: {0}")]
    Join(#[from] tokio::task::JoinError),
}

// Retry/reconnect stance: none inside the store. Open once at boot; transient
// failures map to StoreError → Error::Store → Reply Error(StoreFailure) at the
// execute() boundary. A wedged SQLite is boot-restart territory in v0.
