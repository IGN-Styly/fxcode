//! The ONE place that mints ids (fxproto never generates — see its ids.rs rules).
//!
//! Production: stateless uuid-v7 calls. Tests: deterministic counter mode so
//! FakeAgent integration assertions can expect exact ids ("t-000042").

// Imports to restore as you define the types:
// use std::sync::atomic::{AtomicU64, Ordering};
// use std::sync::Arc;
//
// use fxproto::ids::{AgentId, RequestId, TurnId};

// TODO:
//
// /// Mints every id fxcore ever creates. Cheap to clone; Send + Sync (spawned
// /// turn tasks mint RequestIds across await points).
// ///
// /// Derives: Clone (manual or derived — see interior Arc), Debug.
// /// NOT Copy; no PartialEq needed.
// pub struct IdGen {
//     /// None ⇒ uuid v7 per call (production). Some(prefix) ⇒ "{prefix}-{n:06}"
//     /// from the shared counter (tests).
//     deterministic: Option<String>,
//     /// ONE counter shared by ALL kinds in deterministic mode: uniqueness is
//     /// global, so tests predict sequences by counting mints in the scenario,
//     /// not per-kind. Arc because cloned IdGens must share the sequence.
//     counter: Arc<AtomicU64>,
// }
//
// impl IdGen {
//     /// Production mode: each call formats `uuid::Uuid::now_v7().to_string()`
//     /// — plain hyphenated lowercase (Uuid's Display default). Pin this form:
//     /// wire goldens and BTreeMap iteration order depend on it.
//     pub fn production() -> Self;
//
//     /// Tests only. Every mint renders "{prefix}-{n:06}" where n is fetched
//     /// with `fetch_add(1, Relaxed)` on the SHARED counter starting at 0
//     /// (first id is "{prefix}-000000"). Different id KINDS share n and the
//     /// prefix — a turn after two agents under deterministic("t") is still
//     /// "t-000002". Tests assert by simulating exact interleavings, which the
//     /// serial actor loop makes predictable.
//     pub fn deterministic(prefix: &str) -> Self;
//
//     fn next_raw(&self) -> String;   // private: switches on `deterministic`
//
//     /// Typed ctors — the ONLY way to obtain a minted id:
//     pub fn agent(&self) -> AgentId;      // StartAgent success (cmd/session.rs)
//     pub fn turn(&self) -> TurnId;        // per Prompt (cmd/session.rs)
//     pub fn request(&self) -> RequestId;  // inbound permission (acp/normalize.rs)
// }
//
// // NEGATIVE-TEST NOTE — SessionId/ToolCallId are ADOPTED, never minted
// // (fxproto/src/ids.rs WHO-MINTS-WHAT table; OptionId likewise). IdGen has
// // deliberately NO session()/tool_call()/option() ctor. Enforcement is a
// // compile-fail doctest attached to this impl block (works because `pub mod
// // ids` makes the path reachable even though lib.rs does not re-export IdGen):
// //
// //   /// ```compile_fail
// //   /// let g = fxcore::ids::IdGen::production();
// //   /// let _ = g.session();    // MUST NOT compile: sessions come from agents
// //   /// ```
// //   (add sibling examples for tool_call()/option() when writing real code).
// // Adding any of those ctors later is a bug: upsert semantics and future
// // session/load-resume both depend on agent-side ids passing through verbatim.
//
// Wiring: exactly ONE IdGen lives inside Orchestrator (orchestrator.rs:
// production() normally, deterministic injection via Orchestrator::new_with_ids
// for tests). normalize.rs receives an owned clone at AcpConnection::start time
// so permission requests mint RequestIds at the boundary without touching the
// orchestrator — cloning is cheap in BOTH modes (stateless / Arc'd atomic).
//
// DEPENDENCY FLAG (not fixable in this file): requires `uuid = { version = "1",
// features = ["v7"] }` in crates/fxcore/Cargo.toml (workspace deps preferred).
// The workspace Cargo.lock already carries uuid transitively; make it explicit
// when implementing. fxproto must NEVER gain this dep (its ids.rs rules).
