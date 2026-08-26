//! Event persistence contract.

// TODO:
//
// use fxproto::event::{FxEvent, Sequenced};
// use crate::ids::Seq;
//
//
// #[async_trait? or plain fn returning futures — decide; rusqlite is sync so a
// blocking task bridge is needed either way]
// pub trait EventStore: Send + Sync {
//     /// Persist and stamp seq. MUST be totally ordered appends (see sqlite.rs).
//     async fn append(&self, ev: FxEvent) -> Result<Sequenced<FxEvent>, StoreError>;
//     /// All events strictly after `after`, ascending. Empty when cursor == head.
//     async fn replay(&self, after: Seq) -> Result<Vec<Sequenced<FxEvent>>, StoreError>;
//     /// Current max seq (0 for empty log).
//     async fn head_seq(&self) -> Result<Seq, StoreError>;
// }
//
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite failure: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization failure: {0}")]
    Serde(#[from] serde_json::Error),
}
