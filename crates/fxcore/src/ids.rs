//! The ONE place that mints ids (fxproto never generates — see its ids.rs rules).
//!
//! Production: stateless uuid-v7 calls. Tests: deterministic counter mode so
//! FakeAgent integration assertions can expect exact ids ("t-000042").

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use fxproto::ids::{AgentId, RequestId, TurnId};

/// Mints every id fxcore ever creates. Cheap to clone; Send + Sync (spawned
/// turn tasks mint RequestIds across await points).
///
/// NOT Copy; no PartialEq needed.
#[derive(Clone, Debug)]
pub struct IdGen {
    /// None ⇒ uuid v7 per call (production). Some(prefix) ⇒ "{prefix}-{n:06}"
    /// from the shared counter (tests).
    deterministic: Option<String>,
    /// ONE counter shared by ALL kinds in deterministic mode: uniqueness is
    /// global, so tests predict sequences by counting mints in the scenario,
    /// not per-kind. Arc because cloned IdGens must share the sequence.
    counter: Arc<AtomicU64>,
}

impl IdGen {
    /// Production mode: each call formats `uuid::Uuid::now_v7().to_string()`
    /// — plain hyphenated lowercase (Uuid's Display default). Pin this form:
    /// wire goldens and BTreeMap iteration order depend on it.
    pub fn production() -> Self {
        Self {
            deterministic: None,
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Tests only. Every mint renders "{prefix}-{n:06}" where n is fetched
    /// with `fetch_add(1, Relaxed)` on the SHARED counter starting at 0
    /// (first id is "{prefix}-000000"). Different id KINDS share n and the
    /// prefix — a turn after two agents under deterministic("t") is still
    /// "t-000002". Tests assert by simulating exact interleavings, which the
    /// serial actor loop makes predictable.
    pub fn deterministic(prefix: &str) -> Self {
        Self {
            deterministic: Some(prefix.to_owned()),
            counter: Arc::new(AtomicU64::new(0)),
        }
    }

    fn next_raw(&self) -> String {
        match &self.deterministic {
            Some(prefix) => {
                let n = self.counter.fetch_add(1, Ordering::Relaxed);
                format!("{prefix}-{n:06}")
            }
            None => uuid::Uuid::now_v7().to_string(),
        }
    }

    /// Typed ctors — the ONLY way to obtain a minted id:
    pub fn agent(&self) -> AgentId {
        AgentId::from_raw(self.next_raw())
    }

    pub fn turn(&self) -> TurnId {
        TurnId::from_raw(self.next_raw())
    }

    pub fn request(&self) -> RequestId {
        RequestId::from_raw(self.next_raw())
    }

    // NEGATIVE-TEST NOTE — SessionId/ToolCallId are ADOPTED, never minted
    // (fxproto/src/ids.rs WHO-MINTS-WHAT table; OptionId likewise). IdGen has
    // deliberately NO session()/tool_call()/option() ctor:
    //
    //   /// ```compile_fail
    //   /// let g = fxcore::ids::IdGen::production();
    //   /// let _ = g.session();    // MUST NOT compile: sessions come from agents
    //   /// ```
    //
    // Adding any of those ctors later is a bug: upsert semantics and future
    // session/load-resume both depend on agent-side ids passing through verbatim.
}
