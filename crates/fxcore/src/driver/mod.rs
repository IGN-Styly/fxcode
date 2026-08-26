//! Driver registry + detection: DriverId → how to spawn the agent.

pub mod acp;
pub mod detect;

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
// use std::sync::Mutex;
//
// use fxproto::driver::{DriverId, DriverSpec};
// use fxproto::reply::DetectedDriver;
//
// use crate::driver::detect::Detection;

// TODO:
//
// /// Everything needed to spawn one agent binary, after config + autodetect resolve.
// pub struct SpawnPlan {
//     pub driver: DriverId,
//     pub spec: DriverSpec,
//     /// What detect.rs actually resolved on disk (PATH result / known-location
//     /// hit / config-override program). None = no candidate found; AcpConnection::
//     /// start then passes spec.program to tokio Command raw and PATH resolution
//     /// happens inside the OS at spawn time (last resort before failing).
//     pub resolved_program: Option<std::path::PathBuf>,
//     pub detected_version: Option<String>,   // None = unverified (still allowed to try)
// }
//
// /// DriverId → detection/plan authority. Holds Config overrides and memoizes
// /// detection so plan() never hits the disk twice for the same driver.
// ///
// /// Cache policy (DECIDED): BOTH hits and misses are cached; there is no
// /// invalidation in v0. Rationale: detect_all runs only on Command::DetectAgents
// /// and plan() only at StartAgent time; agents do not hot-install mid-session
// /// often enough to matter, and fxserver restart refreshes everything anyway.
// /// If M3 setup UX needs "re-scan", add invalidate(DriverId) then — not now.
// ///
// /// Concurrency note: the cache is a sync Mutex because the critical section is
// /// pure map reads/writes; the PROCESS work (detect()) happens OUTSIDE the lock
// /// (read-miss → detect().await → re-check → insert), so no await is ever held
// /// across a lock guard.
// pub struct DriverRegistry {
//     overrides: BTreeMap<DriverId, DriverSpec>,   // from Config (config.rs)
//     cache: Mutex<BTreeMap<DriverId, Option<Detection>>>,  // None = detected-miss
// }
//
// impl DriverRegistry {
//     pub fn new(overrides: BTreeMap<DriverId, DriverSpec>) -> Self;
//
//     /// Resolve the full spawn story WITHOUT hitting the disk twice:
//     ///   1. override present    => Detection::from_override (probe only) — cached
//     ///   2. cache hit           => return immediately
//     ///   3. otherwise           => detect::detect(id, None).await, insert, return
//     /// Errors bubble from probe failures? No — detection is total (absence is
//     /// data); Err is reserved for poisoned-cache poisoning, surfaced as
//     /// crate::Error::AgentStart (lib.rs).
//     pub async fn plan(&self, id: DriverId) -> Result<SpawnPlan, crate::Error>;
//
//     /// Used by Command::DetectAgents — checks all three drivers. Returns rows in
//     /// DriverId declaration order (ClaudeCode, GeminiCli, CodexCli): stable UI
//     /// ordering, asserted by tests/orchestrator.rs detect_agents test.
//     /// Each row = DetectedDriver from detect.rs (found:false rows are DATA —
//     /// command.rs pins that DetectAgents itself never errors).
//     pub async fn detect_all(&self) -> Vec<DetectedDriver>;
//
//     /// Unit-test seam + future invalidation point: seed/replace one driver's
//     /// resolved plan without spawning anything (tests/orchestrator.rs uses this
//     /// to inject FakeAgent-backed SpawnPlans).
//     #[cfg(test)]
//     pub fn set_plan_for_tests(&mut self, id: DriverId, plan: SpawnPlan);
// }
