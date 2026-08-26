//! Driver registry + detection: DriverId → how to spawn the agent.

pub mod acp;
pub mod detect;

// Imports to restore as you define the types:
// use std::collections::BTreeMap;
// use fxproto::driver::{DriverId, DriverSpec};

// TODO:
//
// /// Everything needed to spawn one agent binary, after config + autodetect resolve.
// pub struct SpawnPlan {
//     pub driver: DriverId,
//     pub spec: DriverSpec,
//     pub detected_version: Option<String>,   // None = unverified (still allowed to try)
// }
//
// pub struct DriverRegistry {
//     overrides: BTreeMap<DriverId, DriverSpec>,   // from Config
//     cache: Mutex<BTreeMap<DriverId, Option<DetectedDriver>>>,  // detection memoized
// }
//
// impl DriverRegistry {
//     pub fn new(overrides: ...) -> Self;
//     /// Resolve spec for a driver WITHOUT hitting the disk twice (cache).
//     pub async fn plan(&self, id: DriverId) -> Result<SpawnPlan>;
//     /// Used by Command::DetectAgents — checks all known drivers.
//     pub async fn detect_all(&self) -> Vec<DetectedDriver>;
// }
