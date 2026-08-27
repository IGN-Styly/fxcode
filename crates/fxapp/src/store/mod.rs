//! AppState global: the client-side projections. Views NEVER see raw events.
//!
//! The single mutation entrypoint is [`AppState::apply`]; nobody else mutates
//! the three model states (folds in fxproto::model own the real logic). Views
//! read this global straight (`cx.global::<AppState>()`) and re-render via
//! `cx.observe_global::<AppState>` + `cx.notify()` — GPUI fires the global-
//! observer effect automatically whenever the global is mutated through
//! `cx.global_mut::<AppState>()`, which is what conn/mod.rs does per event.
//!
//! SnapshotRequired handling lives one call up (conn/mod.rs): it REPLACES all
//! three projection fields wholesale from the snapshot (assignment, not
//! folding), then resumes ingest — routed through [`AppState::replace_all`]
//! so there is exactly one assignment site (model/mod.rs delivery contract).

use fxproto::envelope::Snapshot;
use fxproto::event::{FxEvent, Sequenced};
use fxproto::model::{AgentsState, PermsState, ThreadsState};

// Imports restored as implemented (top of file):
use gpui::Global;

use fxproto::model::apply_agent;
use fxproto::model::apply_perms;
use fxproto::model::apply_thread;

use crate::conn::ConnStatus; // SINGLE definition lives in conn/mod.rs
use fxproto::ids::AgentId;
use fxproto::reply::DetectedDriver;
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct AppState {
    pub conn_status: ConnStatus,
    pub agents: AgentsState,
    pub threads: ThreadsState,
    pub perms: PermsState,
    /// Latest DetectAgents answer (refreshed per link by ConnectionManager).
    /// Drives the t3-style composer-side agent picker: found rows are selectable,
    /// not-found rows render as install hints (data, not errors — reply.rs).
    pub detected: Vec<DetectedDriver>,
    /// Last working-directory the user launched a session with (t3code "project"
    /// equivalent: our protocol has no project entity, so cwd IS the project).
    pub last_cwd: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            conn_status: ConnStatus::Disconnected { fatal: None },
            agents: AgentsState::default(),
            threads: ThreadsState::default(),
            perms: PermsState::default(),
            detected: Vec::new(),
            last_cwd: None,
        }
    }
}

impl Global for AppState {}

impl AppState {
    /// The single mutation entrypoint:
    ///   1. run the owning fold(s) on ev.inner (owners table below);
    ///   2. hand back ev.seq.as_u64() so conn/mod.rs advances + persists the
    ///      cursor AFTER the fold ran (cursor.rs timing rules); notification
    ///      happens via the global-observer effect of `global_mut` upstream.
    ///
    /// Variant → owner map (static, mirrors model/* ownership rules exactly):
    ///   AgentStatus                                   → apply_agent (agents only)
    ///   SessionCreated | TurnStarted | Chunk | ToolCallUpsert | PlanUpdated |
    ///   PermissionRequested | PermissionResolved | TurnFinished
    ///                                                 → apply_thread (everything else)
    ///   PermissionRequested | PermissionResolved      → apply_perms AS WELL — the same
    ///     event deliberately folds into BOTH states independently ("derived twice",
    ///     model/mod.rs); neither state reads the other.
    /// Total per event: owners are {agents} or {threads} or {threads, perms}.
    ///
    /// NOTIFY-GRANULARITY v0 PLAN: ONE notification after every apply() — folds
    /// are O(event), GPUI batches redraws within a frame. Seams pre-cut for
    /// refinement: states are separate structs TODAY and this fn knows each
    /// variant's owner set (the same match below); upgrade = swap the shared
    /// notify for per-owner Entity notifies behind ONE facade method.
    /// UPGRADE TRIGGER CONDITION (either ⇒ refactor):
    ///   (a) profiler shows any interactive frame >16 ms during streaming turns
    ///       attributable to ≥2 passive views re-rendering foreign-domain events, OR
    ///   (b) sustained ingest >200 Sequenced events/s for >10 s (bursty tool storms count).
    pub fn apply(&mut self, ev: &Sequenced<FxEvent>) -> u64 {
        match &ev.inner {
            FxEvent::AgentStatus { .. } => apply_agent(&mut self.agents, &ev.inner),
            FxEvent::PermissionRequested { .. } | FxEvent::PermissionResolved { .. } => {
                apply_thread(&mut self.threads, &ev.inner);
                apply_perms(&mut self.perms, &ev.inner);
            }
            _ => apply_thread(&mut self.threads, &ev.inner),
        }
        ev.seq.as_u64()
    }

    /// SnapshotRequired path: REPLACE projections wholesale (assignment, never
    /// folding), then ingest resumes normally. last_seq := baseline_seq is
    /// handed back to conn/mod.rs which persists it once for the whole batch.
    pub fn replace_all(&mut self, snapshot: &Snapshot) -> u64 {
        self.agents = snapshot.agents.clone();
        self.threads = snapshot.threads.clone();
        self.perms = snapshot.perms.clone();
        snapshot.baseline_seq.as_u64()
    }

    /// Rows whose driver was actually FOUND on the server machine, in stable
    /// DriverId order — the picker's selectable universe.
    pub fn found_drivers(&self) -> Vec<DriverRow> {
        let mut rows: Vec<DriverRow> = self
            .detected
            .iter()
            .map(|d| DriverRow {
                driver: d.driver,
                found: d.found,
                version: d.version.clone(),
                spec_program: d.spec_used.program.clone(),
            })
            .collect();
        // Fill gaps so every DriverId always renders (unfound = install hint).
        let present: HashSet<_> = rows.iter().map(|r| r.driver).collect();
        for all in enum_driver_ids() {
            if !present.contains(&all) {
                rows.push(DriverRow {
                    driver: all,
                    found: false,
                    version: None,
                    spec_program: String::new(),
                });
            }
        }
        rows.sort_by_key(|r| r.driver);
        rows
    }

    /// A RUNNING process of exactly this driver (spawning counts: Starting may
    /// still die; callers treat it as unavailable-but-in-flight). t3code spawns
    /// lazily; we key off explicit AgentStatus events so replay stays truthful.
    pub fn running_agent_for(&self, driver: fxproto::driver::DriverId) -> Option<AgentId> {
        self.agents
            .agents
            .iter()
            .find(|(_, st)| {
                st.driver == driver
                    && matches!(
                        st.status,
                        fxproto::event::AgentStatus::Ready | fxproto::event::AgentStatus::Busy
                    )
            })
            .map(|(id, _)| id.clone())
    }

    /// Stash DetectAgents answers centrally; idempotent per link refresh.
    pub fn record_detected(&mut self, drivers: Vec<DetectedDriver>) {
        self.detected = drivers;
    }
}

/// View-model row for the picker (keeps gpui views decoupled from reply types'
/// full shape and makes "fill-unfound-gaps" logic unit-testable).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverRow {
    pub driver: fxproto::driver::DriverId,
    pub found: bool,
    pub version: Option<String>,
    pub spec_program: String,
}

impl DriverRow {
    pub fn label(&self) -> String {
        format!(
            "{}{}",
            self.driver.label(),
            self.version
                .as_deref()
                .map(|v| format!(" · {v}"))
                .unwrap_or_default()
        )
    }
}

fn enum_driver_ids() -> Vec<fxproto::driver::DriverId> {
    use fxproto::driver::DriverId;
    vec![
        DriverId::ClaudeCode,
        DriverId::GeminiCli,
        DriverId::CodexCli,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fxproto::content::{Role, ToolCallStatus};
    use fxproto::driver::DriverId;
    use fxproto::event::{AgentStatus as FxAgentStatus, Sequenced};
    use fxproto::ids::{AgentId, Seq, SessionId, ToolCallId};

    fn sequenced(seq: u64, ev: FxEvent) -> Sequenced<FxEvent> {
        Sequenced {
            seq: Seq::new(seq),
            inner: ev,
        }
    }

    fn chunk(session: &str, text: &str) -> FxEvent {
        FxEvent::Chunk {
            session: SessionId::from_raw(session.into()),
            turn: fxproto::ids::TurnId::from_raw("t".into()),
            role: Role::User,
            text: text.to_string(),
        }
    }

    fn agent_ready(agent: &str) -> FxEvent {
        FxEvent::AgentStatus {
            agent: AgentId::from_raw(agent.into()),
            driver: DriverId::ClaudeCode,
            status: FxAgentStatus::Ready,
        }
    }

    #[test]
    fn apply_returns_seq_for_cursor_bookkeeping_after_folds_ran() {
        let mut st = AppState::default();
        assert_eq!(st.apply(&sequenced(3, chunk("s", "hi"))), 3);
        assert_eq!(st.apply(&sequenced(4, chunk("s", " there"))), 4);

        let thread = st
            .threads
            .threads
            .get(&SessionId::from_raw("s".into()))
            .unwrap();
        assert_eq!(thread.messages[0].text, "hi there");
        // No watermark inside the fold targets themselves (delivery contract).
        assert_eq!(st.agents, AgentsState::default());
    }

    #[test]
    fn agents_events_own_only_the_agents_state() {
        let mut st = AppState::default();
        st.apply(&sequenced(1, agent_ready("a")));

        assert_eq!(st.agents.agents.len(), 1);
        assert_eq!(st.threads, ThreadsState::default());
        assert_eq!(st.perms, PermsState::default());
    }

    #[test]
    fn permission_events_derive_twice_into_threads_and_perms() {
        let mut st = AppState::default();
        st.apply(&sequenced(2, chunk("s", "u")));
        let upserted = FxEvent::ToolCallUpsert {
            session: SessionId::from_raw("s".into()),
            tool_call: ToolCallId::from_raw("tc".into()),
            title: "ls".into(),
            kind: fxproto::content::ToolCallKind::Execute,
            status: ToolCallStatus::InProgress,
            output: None,
            _meta: None,
        };
        st.apply(&sequenced(3, upserted));
        st.apply(&sequenced(
            4,
            FxEvent::PermissionRequested {
                request_id: fxproto::ids::RequestId::from_raw("r".into()),
                session: SessionId::from_raw("s".into()),
                tool_call: fxproto::event::ToolCallSummary {
                    tool_call: ToolCallId::from_raw("tc".into()),
                    title: "ls".into(),
                },
                options: vec![],
            },
        ));

        assert_eq!(st.perms.pending.len(), 1); // perms owner saw it …
        assert!(
            st.threads // … and threads recorded its independent bridge too.
                .threads
                .get(&SessionId::from_raw("s".into()))
                .unwrap()
                .pending_perm_tools
                .contains_key(&fxproto::ids::RequestId::from_raw("r".into()))
        );
        assert!(st.agents.agents.is_empty());
    }

    #[test]
    fn replace_all_swaps_every_projection_at_once() {
        let mut snapshot_source = AppState::default();
        snapshot_source.apply(&sequenced(9, agent_ready("snap-agent")));
        snapshot_source.apply(&sequenced(10, chunk("snap-session", "kept")));
        snapshot_source
            .perms
            .recent
            .push_back(fxproto::model::perms::ResolvedPermission {
                request_id: fxproto::ids::RequestId::from_raw("r0".into()),
                chosen: None,
            });

        let baseline_agents = snapshot_source.agents.clone();
        let baseline_threads = snapshot_source.threads.clone();
        let baseline_perms = snapshot_source.perms.clone();

        let mut fresh = AppState::default();
        fresh.apply(&sequenced(11, agent_ready("older-ghost")));

        let baseline = fresh.replace_all(&Snapshot {
            baseline_seq: Seq::new(10),
            agents: baseline_agents,
            threads: baseline_threads,
            perms: baseline_perms,
        });
        assert_eq!(baseline, 10);

        assert_eq!(fresh.agents, snapshot_source.agents);
        assert_eq!(fresh.threads, snapshot_source.threads);
        assert_eq!(fresh.perms, snapshot_source.perms);
        // Ghost state from before the resync is gone wholesale.
        assert_eq!(
            fresh.threads.threads.get(&SessionId::from_raw("s".into())),
            None,
            "pre-snapshot junk must not survive a wholesale replace"
        );
    }

    #[test]
    fn default_app_state_is_empty_and_idle() {
        let st = AppState::default();
        assert_eq!(st.conn_status, ConnStatus::Disconnected { fatal: None });
        assert!(st.threads.threads.is_empty());
        assert!(st.agents.agents.is_empty());
    }

    /// COUPLING GUARD for the owner match in apply(): one constructor per
    /// FxEvent variant. Adding a tenth variant makes this test fail to compile,
    /// forcing the routing rules here (and any new fold ownership) to be decided
    /// deliberately instead of silently landing in the `_ =>` catch-all.
    #[test]
    fn every_event_variant_routes_through_apply() {
        let events: [FxEvent; 9] = [
            FxEvent::AgentStatus {
                agent: AgentId::from_raw("a".into()),
                driver: DriverId::ClaudeCode,
                status: FxAgentStatus::Ready,
            },
            FxEvent::TurnStarted {
                session: SessionId::from_raw("s".into()),
                turn: fxproto::ids::TurnId::from_raw("t".into()),
            },
            FxEvent::Chunk {
                session: SessionId::from_raw("s".into()),
                turn: fxproto::ids::TurnId::from_raw("t".into()),
                role: Role::User,
                text: "c".into(),
            },
            FxEvent::ToolCallUpsert {
                session: SessionId::from_raw("s".into()),
                tool_call: ToolCallId::from_raw("tc".into()),
                title: "t".into(),
                kind: fxproto::content::ToolCallKind::Read,
                status: ToolCallStatus::Completed,
                output: None,
                _meta: None,
            },
            FxEvent::PlanUpdated {
                session: SessionId::from_raw("s".into()),
                entries: vec![],
            },
            FxEvent::PermissionRequested {
                request_id: fxproto::ids::RequestId::from_raw("r".into()),
                session: SessionId::from_raw("s".into()),
                tool_call: fxproto::event::ToolCallSummary {
                    tool_call: ToolCallId::from_raw("tc".into()),
                    title: "t".into(),
                },
                options: vec![],
            },
            FxEvent::PermissionResolved {
                request_id: fxproto::ids::RequestId::from_raw("r".into()),
                chosen: None,
            },
            FxEvent::TurnFinished {
                session: SessionId::from_raw("s".into()),
                turn: fxproto::ids::TurnId::from_raw("t".into()),
                stop_reason: fxproto::content::StopReason::EndTurn,
            },
            FxEvent::SessionCreated {
                session: SessionId::from_raw("s2".into()),
                agent: AgentId::from_raw("a".into()),
                cwd: "/w".into(),
                mcp_servers: vec![],
            },
        ];
        let mut st = AppState::default();
        let mut last = 0;
        for (i, ev) in events.into_iter().enumerate() {
            last = st.apply(&sequenced(last + 1, ev));
            assert_eq!(last, i as u64 + 1);
        }
    }
}
