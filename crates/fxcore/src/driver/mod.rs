//! Driver registry + detection: DriverId → how to spawn the agent.

pub mod acp;
pub mod detect;

use std::collections::BTreeMap;
use std::sync::Mutex;

use fxproto::driver::{DriverId, DriverSpec};
use fxproto::reply::DetectedDriver;

use crate::driver::detect::Detection;

/// Everything needed to spawn one agent binary, after config + autodetect resolve.
#[derive(Debug, Clone)]
pub struct SpawnPlan {
    pub driver: DriverId,
    pub spec: DriverSpec,
    /// What detect.rs actually resolved on disk (PATH result / known-location
    /// hit / config-override program). None = no candidate found; AcpConnection::
    /// start then passes spec.program to tokio Command raw and PATH resolution
    /// happens inside the OS at spawn time (last resort before failing).
    pub resolved_program: Option<std::path::PathBuf>,
    pub detected_version: Option<String>, // None = unverified (still allowed to try)
}

/// DriverId → detection/plan authority. Holds Config overrides and memoizes
/// detection so plan() never hits the disk twice for the same driver.
///
/// Cache policy (DECIDED): BOTH hits and misses are cached; there is no
/// invalidation in v0. Rationale: detect_all runs only on Command::DetectAgents
/// and plan() only at StartAgent time; agents do not hot-install mid-session
/// often enough to matter, and fxserver restart refreshes everything anyway.
///
/// Concurrency note: the cache is a sync Mutex because the critical section is
/// pure map reads/writes; the PROCESS work (detect()) happens OUTSIDE the lock
/// (read-miss → detect().await → re-check → insert), so no await is ever held
/// across a lock guard.
#[derive(Debug)]
pub struct DriverRegistry {
    overrides: BTreeMap<DriverId, DriverSpec>,
    cache: Mutex<BTreeMap<DriverId, Option<Detection>>>,
}

impl DriverRegistry {
    pub fn new(overrides: BTreeMap<DriverId, DriverSpec>) -> Self {
        Self {
            overrides,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    fn override_for(&self, id: DriverId) -> Option<&DriverSpec> {
        self.overrides.get(&id)
    }

    /// Resolve the full spawn story WITHOUT hitting the disk twice:
    ///   1. override present    => Detection::from_override (probe only) — cached
    ///   2. cache hit           => return immediately
    ///   3. otherwise           => detect::detect(id, None).await, insert, return
    ///
    /// Errors bubble from probe failures? No — detection is total (absence is
    /// data); Err is reserved for poisoned-cache poisoning, surfaced as
    /// crate::Error::AgentStart (lib.rs).
    pub async fn plan(&self, id: DriverId) -> Result<SpawnPlan, crate::Error> {
        // Fast path: a cached entry (hit AND miss) ends the story immediately.
        if let Some(detected) = self
            .cache
            .lock()
            .map_err(|_| poison(id))?
            .get(&id)
            .cloned()
            .flatten()
        {
            return Ok(spawn_plan(detected));
        }

        let detected = match self.override_for(id).cloned() {
            Some(spec) => detect::detect(id, Some(&spec)).await,
            None => detect::detect(id, None).await,
        };

        let mut cache = self.cache.lock().map_err(|_| poison(id))?;
        Ok(match cache.get(&id).cloned().flatten() {
            // Another task raced us to insert; its result wins deterministically.
            Some(raced) => spawn_plan(raced),
            None => {
                cache.insert(id, Some(detected.clone()));
                spawn_plan(detected)
            }
        })
    }

    /// Used by Command::DetectAgents — checks all three drivers. Returns rows in
    /// DriverId declaration order (ClaudeCode, GeminiCli, CodexCli): stable UI
    /// ordering, asserted by tests/orchestrator.rs detect_agents test.
    /// Each row = DetectedDriver from detect.rs (found:false rows are DATA —
    /// command.rs pins that DetectAgents itself never errors).
    pub async fn detect_all(&self) -> Vec<DetectedDriver> {
        for id in [
            DriverId::ClaudeCode,
            DriverId::GeminiCli,
            DriverId::CodexCli,
        ] {
            if self.plan(id).await.is_err() {
                tracing::warn!(target: "detect", ?id, "detection plan poisoned; reporting default");
            }
        }
        let cache = self.cache.lock().ok();
        [
            DriverId::ClaudeCode,
            DriverId::GeminiCli,
            DriverId::CodexCli,
        ]
        .into_iter()
        .map(|id| match &cache {
            Some(cache) => {
                cache
                    .get(&id)
                    .cloned()
                    .flatten()
                    .unwrap_or_else(|| fallback_detection(id))
                    .report
            }
            None => fallback_detection(id).report,
        })
        .collect()
    }

    /// Unit-test seam + future invalidation point: seed/replace one driver's
    /// resolved plan without spawning anything (tests/orchestrator.rs uses this
    /// to inject FakeAgent-backed SpawnPlans).
    #[cfg(test)]
    #[doc(hidden)]
    pub fn set_plan_for_tests(&mut self, id: DriverId, plan: SpawnPlan) {
        self.overrides.remove(&id);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                id,
                Some(Detection {
                    report: DetectedDriver {
                        driver: id,
                        found: true,
                        version: plan.detected_version.clone(),
                        spec_used: plan.spec.clone(),
                    },
                    resolved_program: plan.resolved_program.clone(),
                }),
            );
        }
    }
}

fn fallback_detection(id: DriverId) -> Detection {
    Detection {
        report: DetectedDriver {
            driver: id,
            found: false,
            version: None,
            spec_used: id.default_spec(),
        },
        resolved_program: None,
    }
}

fn spawn_plan(detection: Detection) -> SpawnPlan {
    let Detection {
        report:
            DetectedDriver {
                driver,
                version,
                spec_used,
                ..
            },
        resolved_program,
    } = detection;
    SpawnPlan {
        driver,
        spec: spec_used,
        resolved_program,
        detected_version: version,
    }
}

fn poison(id: DriverId) -> crate::Error {
    crate::Error::AgentStart(format!("driver registry cache poisoned for {id:?}"))
}
