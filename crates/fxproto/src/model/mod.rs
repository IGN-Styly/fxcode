//! Canonical projections + fold functions — the shared brain.
//!
//! BOTH sides run these:
//! - fxserver (fxcore/src/proj.rs): rebuilds state at boot by folding the event log,
//!   and uses it to validate commands (e.g. reject Prompt for unknown session).
//! - fxapp (src/store/mod.rs): applies live events to UI stores.
//!
//! Contract rules:
//!
//! - Signature: `fn apply_*(state: &mut XState, ev: &FxEvent)` — plain `&FxEvent`,
//!   NOT `&Sequenced<FxEvent>`: folds never read `seq`. Cursor bookkeeping belongs
//!   to callers (fxcore `Projections::apply` post-append; fxapp `AppState::apply`
//!   before persisting `last_seq`). An earlier draft of this file said `Sequenced`;
//!   the three per-file signatures were the majority — this line now matches them.
//! - Ownership: the three `apply_*` fns are the ONLY mutators of these states on
//    either side. Callers pass states in and render them; they never poke fields.
//! - Folds are TOTAL: any event applied to any state is defined, never panics.
//!   Unknown parents follow two fixed policies (below), never ad-hoc choices.
//! - Delivery contract: both callers consume each `Sequenced<FxEvent>` EXACTLY ONCE
//!   (replay-from-cursor then live attach; `SnapshotRequired` REPLACES state rather
//!   than replaying overlap). Folds therefore carry no applied-seq watermarks, and
//    "idempotent" in the per-file maps means: re-applying KEYED/upsert-shaped
//!   events is a no-op. Append-shaped payloads (Chunk text merges, sessions-list
//!   pushes) WOULD duplicate under double-apply — that is expected and covered by
//!   the exactly-once contract, not by fold logic. impl.md Phase 1.2's "re-applying
//!   same event => no dupes" must be read as scoped to keyed events; the checklist
//!   below encodes exactly that scope.
//! - Auto-vivify policy (threads + perms): any event carrying a `session` ensures
//!   the ThreadState exists first (get-or-create with defaults), so replays that
//!   start mid-session and snapshot baselines still render. AGENTS IS THE EXCEPTION:
//    SessionCreated naming an unknown AgentId is logged and ignored — AgentState
//!   needs a DriverId, which cannot be synthesized, and protocol ordering
//!   (StartAgent -> AgentStatus(Starting) -> NewSession -> SessionCreated) makes a
//!   missing parent a garbled-log symptom where ignoring is least damaging.
//! - States are INDEPENDENT: no apply_* reads another state. `PermissionResolved`
//!   is deliberately derived TWICE — perms.rs records the audit row, threads.rs
//!   stamps the tool-card badge — so replay order across states can never matter
//    and neither side can drift off the other.
//! - Derives: EVERY state type in this module derives `Serialize` + `Deserialize`
//    because envelope.rs `Snapshot` serializes the three top-level states
//    (AgentsState / ThreadsState / PermsState) WHOLE — clients deserialize them
//!   straight into their stores. Top-level states additionally derive `Default`
//    (boot/rebuild fold target), `Clone` + `Debug` (UI/test ergonomics) and
//!   `PartialEq` (checklist asserts state equality). Nested types derive what their
//!   owner needs; each file's TODO block lists them explicitly — do not guess.
//! - Logging levels: unknown-parent / dropped-event => `tracing::debug!`;
//!   anomalies indicating a protocol bug (double TurnFinished, overwritten
//!   active turn) => `tracing::warn!`.
//! - No I/O, no clocks, no randomness — pure functions of (state, event).

pub mod agents;
pub mod perms;
pub mod threads;

// Re-exported public surface (exhaustive — adding a type means adding it here too;
// downstream crates use both `fxproto::model::X` and these):
pub use self::agents::{AgentState, AgentsState, apply_agent};
pub use self::perms::{PendingPermission, PermsState, RECENT_CAP, ResolvedPermission, apply_perms};
pub use self::threads::{
    FlowItem, Message, PermOutcome, ThreadState, ThreadsState, ToolCall, apply_thread,
};

// TODO(tests — impl.md Phase 1.2; run with `cargo test -p fxproto`. Property style:
// one checklist line = at least one test fn. Helpers: fresh default states; ev()
// constructors per variant; apply one event per step.)
//
// agents (apply_agent) — see agents.rs rules S1–S3:
//   A1  AgentStatus on empty state => entry created with event's driver + status,
//       sessions empty.
//   A2  Re-apply identical AgentStatus => state unchanged (PartialEq; keyed
//       idempotence per the delivery contract).
//   A3  Starting -> Ready -> Busy -> Ready sequence => status tracks last event.
//   A4  Crashed { exit_code: Some(-9) } survives serde round-trip.
//   A5  SessionCreated for known agent appends once; re-apply => still once.
//   A6  SessionCreated for UNKNOWN agent => state unchanged (debug log, no
//       placeholder entry).
//   A7  Each of the other seven variants individually => state unchanged.
//   A8  Whole AgentsState serde round-trip (empty + populated) byte-stable.
//
// threads (apply_thread) — see threads.rs rules W0–W8:
//   T1  Chunk for unseen session auto-vivifies the thread; chunk lands at
//       messages[0] / flow[0].
//   T2  Consecutive same-role chunks MERGE into one Message (len == 1, texts
//       concatenated in arrival order).
//   T3  Role flip (User then Agent) starts a NEW Message; text never merges
//       across roles.
//   T4  Chunk AFTER a tool call does not merge into the pre-tool message even
//       when roles match — merge compares ONLY flow.last().
//   T5  ToolCallUpsert BEFORE any message => flow[0] is FlowItem::Tool, messages
//       still empty; nothing synthetic is invented.
//   T6  ToolCallUpsert twice, same id => map holds 1 entry whose fields equal the
//       LATEST event, exactly one flow item at the first-seen position, `perm`
//       preserved across overwrite.
//   T7  Two distinct tool ids => two map entries; flow order == first-appearance
//       order regardless of upsert update order afterwards.
//   T8  TurnStarted sets active_turn; TurnFinished for the SAME turn clears it.
//   T9  Second TurnFinished (already cleared) => warn, state unchanged.
//   T10 TurnFinished for a stale/different turn id => active_turn untouched.
//   T11 PlanUpdated REPLACES wholesale: second update with fewer entries shrinks
//       the plan (no merge ghosts).
//   T12 PermissionResolved with a recorded mapping and an upserted tool =>
//       tool.perm set to Chosen/Cancelled per `chosen`, mapping entry removed;
//       re-apply => no further change.
//   T13 PermissionResolved for an unknown request_id => state unchanged.
//   T14 PermissionRequested records the id bridge even when the tool has not been
//       upserted yet (annotation then skips gracefully per W6).
//   T15 Under a long randomized event mix, messages never shrink (append-only
//       invariant keeps every FlowItem::Message index valid).
//   T16 Whole ThreadsState serde round-trip byte-stable (BTreeMaps give
//       deterministic field order).
//   T17 Fuzz: interleave all nine variants over random sessions in random order
//       => no panic (totality) + invariants T2/T4/T6/T15 hold at the end.
//
// perms (apply_perms) — see perms.rs rules R1–R3:
//   P1  PermissionRequested inserts into pending keyed by request_id.
//   P2  Same request_id requested again => single entry, fields = latest.
//   P3  PermissionResolved removes from pending AND appends to recent carrying
//       `chosen`.
//   P4  chosen = None lands in recent as a Cancelled audit row (never dropped
//       silently).
//   P5  Resolution for a never-requested id => still appended to recent; pending
//       untouched.
//   P6  Inserting RECENT_CAP + 10 resolutions => oldest 10 evicted, newest 50
//       retained in resolution order (bound is exactly RECENT_CAP == 50).
//   P7  Re-applying the same PermissionResolved => recent holds ONE entry for that
//       id (dedupe-then-push); recent is idempotent unlike Chunk.
//   P8  Any of the seven non-permission variants => state unchanged.
//   P9  Whole PermsState serde round-trip byte-stable.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{
        PlanEntry, PlanEntryStatus, Role, StopReason, ToolCallKind, ToolCallStatus,
    };
    use crate::driver::DriverId;
    use crate::event::{
        AgentStatus, FxEvent, PermissionOption, PermissionOptionKind, Sequenced, ToolCallSummary,
    };
    use crate::ids::{AgentId, OptionId, RequestId, Seq, SessionId, ToolCallId, TurnId};
    use std::path::PathBuf;

    // ---- deterministic PRNG (no rand dep allowed in fxproto) ------------------
    struct Xorshift(u64);
    impl Xorshift {
        fn new(seed: u64) -> Self {
            Self(seed.max(1))
        }
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn pick(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    // ---- id helpers ------------------------------------------------------------
    fn agent(s: &str) -> AgentId {
        AgentId::from_raw(s.into())
    }
    fn session(s: &str) -> SessionId {
        SessionId::from_raw(s.into())
    }
    fn turn(s: &str) -> TurnId {
        TurnId::from_raw(s.into())
    }
    fn tool(s: &str) -> ToolCallId {
        ToolCallId::from_raw(s.into())
    }
    fn req(s: &str) -> RequestId {
        RequestId::from_raw(s.into())
    }
    fn opt(s: &str) -> OptionId {
        OptionId::from_raw(s.into())
    }

    // ---- event constructors ----------------------------------------------------
    fn status_ev(a: &str, status: AgentStatus) -> FxEvent {
        FxEvent::AgentStatus {
            agent: agent(a),
            driver: DriverId::ClaudeCode,
            status,
        }
    }
    fn created_ev(s: &str, a: &str) -> FxEvent {
        FxEvent::SessionCreated {
            session: session(s),
            agent: agent(a),
            cwd: PathBuf::from("/w"),
            mcp_servers: vec![],
        }
    }
    fn turn_started(s: &str, t: &str) -> FxEvent {
        FxEvent::TurnStarted {
            session: session(s),
            turn: turn(t),
        }
    }
    fn chunk_ev(s: &str, r: Role, text: &str) -> FxEvent {
        FxEvent::Chunk {
            session: session(s),
            turn: turn("t"),
            role: r,
            text: text.into(),
        }
    }
    fn upsert_ev(s: &str, tc: &str, title: &str, st: ToolCallStatus) -> FxEvent {
        FxEvent::ToolCallUpsert {
            session: session(s),
            tool_call: tool(tc),
            title: title.into(),
            kind: ToolCallKind::Execute,
            status: st,
            output: None,
            _meta: None,
        }
    }
    fn plan_ev(s: &str, entries: Vec<PlanEntry>) -> FxEvent {
        FxEvent::PlanUpdated {
            session: session(s),
            entries,
        }
    }
    fn perm_req(s: &str, tc: &str, r_id: &str) -> FxEvent {
        FxEvent::PermissionRequested {
            request_id: req(r_id),
            session: session(s),
            tool_call: ToolCallSummary {
                tool_call: tool(tc),
                title: "t".into(),
            },
            options: vec![PermissionOption {
                option_id: opt("o"),
                name: "Allow".into(),
                kind: PermissionOptionKind::AllowOnce,
            }],
        }
    }
    fn perm_resolved(r_id: &str, chosen: Option<&str>) -> FxEvent {
        FxEvent::PermissionResolved {
            request_id: req(r_id),
            chosen: chosen.map(opt),
        }
    }
    fn finished(s: &str, t: &str) -> FxEvent {
        FxEvent::TurnFinished {
            session: session(s),
            turn: turn(t),
            stop_reason: StopReason::EndTurn,
        }
    }
    fn other_seven() -> [FxEvent; 7] {
        [
            turn_started("s", "tx"),
            chunk_ev("s", Role::User, "c"),
            upsert_ev("s", "tcx", "title", ToolCallStatus::Pending),
            plan_ev("s", vec![]),
            perm_req("s", "tcx", "rx"),
            perm_resolved("rx", None),
            finished("s", "tx"),
        ]
    }

    fn thread<'a>(ts: &'a ThreadsState, s: &SessionId) -> &'a ThreadState {
        ts.threads.get(s).expect("thread must exist")
    }

    // ========================================================================
    // agents (apply_agent)
    // ========================================================================

    #[test]
    fn a1_agent_status_on_empty_creates_entry_with_driver_and_status() {
        let mut st = AgentsState::default();
        apply_agent(&mut st, &status_ev("a", AgentStatus::Starting));
        let e = st.agents.get(&agent("a")).unwrap();
        assert_eq!(e.driver, DriverId::ClaudeCode);
        assert_eq!(e.status, AgentStatus::Starting);
        assert!(e.sessions.is_empty());
    }

    #[test]
    fn a2_reapplying_identical_status_is_a_noop() {
        let mut st = AgentsState::default();
        apply_agent(&mut st, &status_ev("a", AgentStatus::Ready));
        let before = st.clone();
        apply_agent(&mut st, &status_ev("a", AgentStatus::Ready));
        assert_eq!(st, before);
    }

    #[test]
    fn a3_status_tracks_last_event_through_ready_busy_cycle() {
        let mut st = AgentsState::default();
        apply_agent(&mut st, &status_ev("a", AgentStatus::Starting));
        apply_agent(&mut st, &status_ev("a", AgentStatus::Ready));
        apply_agent(&mut st, &status_ev("a", AgentStatus::Busy));
        apply_agent(&mut st, &status_ev("a", AgentStatus::Ready));
        assert_eq!(
            st.agents.get(&agent("a")).unwrap().status,
            AgentStatus::Ready
        );
    }

    #[test]
    fn a4_crashed_status_survives_serde_round_trip() {
        let st = AgentStatus::Crashed {
            exit_code: Some(-9),
        };
        let json = serde_json::to_string(&st).unwrap();
        assert_eq!(json, r#"{"crashed":{"exit_code":-9}}"#);
        assert_eq!(
            serde_json::from_str::<AgentStatus>(&json).unwrap(),
            AgentStatus::Crashed {
                exit_code: Some(-9)
            }
        );
    }

    #[test]
    fn a5_session_created_for_known_agent_appends_once() {
        let mut st = AgentsState::default();
        apply_agent(&mut st, &status_ev("a", AgentStatus::Starting));
        apply_agent(&mut st, &created_ev("s1", "a"));
        apply_agent(&mut st, &created_ev("s1", "a")); // duplicate
        assert_eq!(
            st.agents.get(&agent("a")).unwrap().sessions,
            vec![session("s1")]
        );
    }

    #[test]
    fn a6_session_created_for_unknown_agent_is_ignored() {
        let mut st = AgentsState::default();
        apply_agent(&mut st, &created_ev("s1", "ghost"));
        // No placeholder entry: nothing materialized anywhere.
        assert_eq!(st.agents.len(), 0);
        assert_eq!(st, AgentsState::default());
    }

    #[test]
    fn a7_other_seven_variants_leave_agents_state_untouched() {
        let mut st = AgentsState::default();
        apply_agent(&mut st, &status_ev("a", AgentStatus::Ready));
        apply_agent(&mut st, &created_ev("keep", "a"));
        let expected = st.clone();
        for ev in other_seven() {
            apply_agent(&mut st, &ev);
            assert_eq!(st, expected, "event mutated AgentsState: {ev:?}");
        }
    }

    #[test]
    fn a8_agents_state_serde_round_trip_is_byte_stable() {
        let mut populated = AgentsState::default();
        apply_agent(&mut populated, &status_ev("b", AgentStatus::Busy));
        apply_agent(
            &mut populated,
            &status_ev("a", AgentStatus::Crashed { exit_code: None }),
        );
        apply_agent(&mut populated, &created_ev("s9", "b"));
        apply_agent(&mut populated, &created_ev("s1", "b"));

        let bytes_populated = serde_json::to_string(&populated).unwrap();
        // Determinism: identical reconstruction yields identical bytes.
        let mut rebuilt = AgentsState::default();
        apply_agent(&mut rebuilt, &status_ev("b", AgentStatus::Busy));
        apply_agent(
            &mut rebuilt,
            &status_ev("a", AgentStatus::Crashed { exit_code: None }),
        );
        apply_agent(&mut rebuilt, &created_ev("s9", "b"));
        apply_agent(&mut rebuilt, &created_ev("s1", "b"));
        assert_eq!(bytes_populated, serde_json::to_string(&rebuilt).unwrap());
        // BTreeMap order: "a" entry serializes before "b" regardless of arrival order.
        let idx_a = bytes_populated.find("\"a\"").unwrap();
        let idx_b = bytes_populated.find("\"b\"").unwrap();
        assert!(idx_a < idx_b);

        let empty_bytes = serde_json::to_string(&AgentsState::default()).unwrap();
        assert_eq!(empty_bytes, r#"{"agents":{}}"#);
        let back: AgentsState = serde_json::from_str(&bytes_populated).unwrap();
        assert_eq!(back, populated);
    }

    // ========================================================================
    // threads (apply_thread)
    // ========================================================================

    #[test]
    fn t1_chunk_auto_vivifies_thread_and_lands_first() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &chunk_ev("s", Role::User, "hi"));
        let ts = thread(&st, &session("s"));
        assert_eq!(
            ts.messages,
            vec![Message {
                role: Role::User,
                text: "hi".into()
            }]
        );
        assert_eq!(ts.flow, vec![FlowItem::Message(0)]);
    }

    #[test]
    fn t2_consecutive_same_role_chunks_merge_in_arrival_order() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &chunk_ev("s", Role::User, "hel"));
        apply_thread(&mut st, &chunk_ev("s", Role::User, "lo "));
        apply_thread(&mut st, &chunk_ev("s", Role::User, "world"));
        let ts = thread(&st, &session("s"));
        assert_eq!(ts.messages.len(), 1);
        assert_eq!(ts.messages[0].text, "hello world");
    }

    #[test]
    fn t3_role_flip_starts_new_message_never_crosses_roles() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &chunk_ev("s", Role::User, "u1"));
        apply_thread(&mut st, &chunk_ev("s", Role::Agent, "a1"));
        apply_thread(&mut st, &chunk_ev("s", Role::Agent, "a2"));
        let ts = thread(&st, &session("s"));
        assert_eq!(
            ts.messages
                .iter()
                .map(|m| (m.role, m.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(Role::User, "u1"), (Role::Agent, "a1a2")]
        );
    }

    #[test]
    fn t4_chunk_after_tool_does_not_merge_across_the_card() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &chunk_ev("s", Role::Agent, "before"));
        apply_thread(
            &mut st,
            &upsert_ev("s", "tc", "ls", ToolCallStatus::InProgress),
        );
        apply_thread(
            &mut st,
            &upsert_ev("s", "tc", "ls", ToolCallStatus::Completed),
        );
        apply_thread(&mut st, &chunk_ev("s", Role::Agent, "after"));
        let ts = thread(&st, &session("s"));
        // Same role as "before" but a Tool card sits between them in flow.last().
        assert_eq!(ts.messages.len(), 2);
        assert_eq!(
            ts.flow,
            vec![
                FlowItem::Message(0),
                FlowItem::Tool(tool("tc")),
                FlowItem::Message(1)
            ]
        );
    }

    #[test]
    fn t5_tool_first_turn_starts_flow_with_tool_no_synthetic_message() {
        let mut st = ThreadsState::default();
        apply_thread(
            &mut st,
            &upsert_ev("s", "tc", "grep", ToolCallStatus::InProgress),
        );
        let ts = thread(&st, &session("s"));
        assert!(ts.messages.is_empty());
        assert_eq!(ts.flow, vec![FlowItem::Tool(tool("tc"))]);
    }

    #[test]
    fn t6_upsert_twice_latest_fields_win_perm_preserved_one_flow_item() {
        let mut st = ThreadsState::default();
        apply_thread(
            &mut st,
            &upsert_ev("s", "tc", "v1", ToolCallStatus::InProgress),
        );
        apply_thread(&mut st, &perm_req("s", "tc", "r1"));
        apply_thread(&mut st, &perm_resolved("r1", Some("o")));
        apply_thread(
            &mut st,
            &upsert_ev("s", "tc", "v2", ToolCallStatus::Completed),
        );

        let ts = thread(&st, &session("s"));
        assert_eq!(ts.tool_calls.len(), 1);
        let card = ts.tool_calls.get(&tool("tc")).unwrap();
        assert_eq!(
            (card.title.as_str(), card.status),
            ("v2", ToolCallStatus::Completed)
        );
        // perm survives the wholesale overwrite (W3).
        assert_eq!(card.perm, Some(PermOutcome::Chosen(opt("o"))));
        assert_eq!(ts.flow, vec![FlowItem::Tool(tool("tc"))]);
    }

    #[test]
    fn t7_two_tools_keep_first_appearance_order_under_late_updates() {
        let mut st = ThreadsState::default();
        apply_thread(
            &mut st,
            &upsert_ev("s", "first", "a", ToolCallStatus::Pending),
        );
        apply_thread(
            &mut st,
            &upsert_ev("s", "second", "b", ToolCallStatus::Pending),
        );
        apply_thread(
            &mut st,
            &upsert_ev("s", "second", "b!", ToolCallStatus::Completed),
        );
        apply_thread(
            &mut st,
            &upsert_ev("s", "first", "a!", ToolCallStatus::Failed),
        );
        let ts = thread(&st, &session("s"));
        assert_eq!(ts.tool_calls.len(), 2);
        assert_eq!(
            ts.flow,
            vec![
                FlowItem::Tool(tool("first")),
                FlowItem::Tool(tool("second"))
            ]
        );
        assert_eq!(ts.tool_calls[&tool("first")].status, ToolCallStatus::Failed);
        assert_eq!(
            ts.tool_calls[&tool("second")].status,
            ToolCallStatus::Completed
        );
    }

    #[test]
    fn t8_turn_started_then_matching_finished_clears_active_turn() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &turn_started("s", "t1"));
        assert_eq!(thread(&st, &session("s")).active_turn, Some(turn("t1")));
        apply_thread(&mut st, &finished("s", "t1"));
        assert_eq!(thread(&st, &session("s")).active_turn, None);
    }

    #[test]
    fn t9_double_turn_finished_absorbed() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &finished("s", "t1")); // finish with no active turn
        apply_thread(&mut st, &turn_started("s", "t1"));
        apply_thread(&mut st, &finished("s", "t1"));
        let before = st.clone();
        apply_thread(&mut st, &finished("s", "t1")); // second
        assert_eq!(st, before);
        assert_eq!(thread(&st, &session("s")).active_turn, None);
    }

    #[test]
    fn t10_stale_turn_finished_leaves_active_turn() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &turn_started("s", "real"));
        let before = st.clone();
        apply_thread(&mut st, &finished("s", "stale"));
        assert_eq!(st, before);
        assert_eq!(thread(&st, &session("s")).active_turn, Some(turn("real")));
    }

    #[test]
    fn t11_plan_replaced_wholesale_shrink_has_no_ghosts() {
        let three = |n| {
            (0..n)
                .map(|i| PlanEntry {
                    content: format!("step{i}"),
                    status: PlanEntryStatus::Pending,
                    priority: None,
                })
                .collect::<Vec<_>>()
        };
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &plan_ev("s", three(3)));
        apply_thread(&mut st, &plan_ev("s", three(1)));
        let ts = thread(&st, &session("s"));
        assert_eq!(ts.plan.len(), 1);
        assert_eq!(ts.plan[0].content, "step0");
    }

    #[test]
    fn t12_resolution_maps_to_chosen_or_cancelled_and_drains_bridge() {
        // Chosen path.
        let mut chosen = ThreadsState::default();
        apply_thread(
            &mut chosen,
            &upsert_ev("s", "tc", "t", ToolCallStatus::InProgress),
        );
        apply_thread(&mut chosen, &perm_req("s", "tc", "r1"));
        apply_thread(&mut chosen, &perm_resolved("r1", Some("o")));
        let card = thread(&chosen, &session("s")).tool_calls[&tool("tc")].clone();
        assert_eq!(card.perm, Some(PermOutcome::Chosen(opt("o"))));
        assert!(thread(&chosen, &session("s")).pending_perm_tools.is_empty());
        let after = chosen.clone();
        apply_thread(&mut chosen, &perm_resolved("r1", Some("o"))); // re-apply
        assert_eq!(chosen, after);

        // Cancelled path (None).
        let mut cancelled = ThreadsState::default();
        apply_thread(
            &mut cancelled,
            &upsert_ev("s", "tc", "t", ToolCallStatus::InProgress),
        );
        apply_thread(&mut cancelled, &perm_req("s", "tc", "r1"));
        apply_thread(&mut cancelled, &perm_resolved("r1", None));
        let card = thread(&cancelled, &session("s")).tool_calls[&tool("tc")].clone();
        assert_eq!(card.perm, Some(PermOutcome::Cancelled));
    }

    #[test]
    fn t13_resolution_unknown_request_id_changes_nothing() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &created_ev("s", "a")); // W0 seed so state differs from default
        let before = st.clone();
        apply_thread(&mut st, &perm_resolved("ghost", Some("o")));
        assert_eq!(st, before);
    }

    #[test]
    fn t14_bridge_recorded_before_tool_upsert_annotation_skips_gracefully() {
        let mut st = ThreadsState::default();
        // Request arrives before any ToolCallUpsert: bridge recorded anyway.
        apply_thread(&mut st, &perm_req("s", "not_yet", "r1"));
        assert_eq!(
            thread(&st, &session("s"))
                .pending_perm_tools
                .get(&req("r1")),
            Some(&tool("not_yet"))
        );
        // Resolution lands while card absent → dropped silently, bridge drained.
        apply_thread(&mut st, &perm_resolved("r1", Some("o")));
        assert!(thread(&st, &session("s")).pending_perm_tools.is_empty());
        assert!(thread(&st, &session("s")).tool_calls.is_empty());
    }

    // ---- invariant checker shared by T15 / T17 --------------------------------

    fn assert_thread_invariants(st: &ThreadsState) {
        for (sid, ts) in &st.threads {
            assert!(ts.messages.len() <= ts.flow.len(), "{sid}");
            for item in &ts.flow {
                match item {
                    FlowItem::Message(i) => assert!(
                        *i < ts.messages.len(),
                        "dangling flow index on {sid}: {i} >= {}",
                        ts.messages.len()
                    ),
                    FlowItem::Tool(id) => {
                        assert!(
                            ts.tool_calls.contains_key(id),
                            "flow references missing card {id}"
                        )
                    }
                }
            }
            // Every map entry has exactly one flow slot (W3 first-appearance push).
            assert_eq!(
                ts.flow
                    .iter()
                    .filter(|f| matches!(f, FlowItem::Tool(_)))
                    .count(),
                ts.tool_calls.len(),
                "flow/tool_calls desynced on {sid}"
            );
        }
    }

    /// W8 smoke: AgentStatus must pass through threads.rs as a pure no-op.
    fn drain_unrelated(mut st: ThreadsState) -> ThreadsState {
        let before = st.clone();
        apply_thread(
            &mut st,
            &FxEvent::AgentStatus {
                agent: agent("other"),
                driver: DriverId::GeminiCli,
                status: AgentStatus::Ready,
            },
        );
        assert_eq!(st, before, "AgentStatus leaked into thread state");
        st
    }

    #[test]
    fn t15_random_mix_messages_are_append_only() {
        let mut rng = Xorshift::new(0xDEADBEEF);
        let mut st = ThreadsState::default();
        let roles = [Role::User, Role::Agent];
        let mut prev_len = [0usize; 2];
        for step in 0..400 {
            let si = rng.pick(2);
            let ev = match rng.pick(5) {
                0 => chunk_ev(
                    if si == 0 { "s0" } else { "s1" },
                    roles[rng.pick(2)],
                    &format!("m{step} "),
                ),
                1..=2 => upsert_ev(
                    if si == 0 { "s0" } else { "s1" },
                    &format!("tc{}", rng.pick(3)),
                    "x",
                    ToolCallStatus::InProgress,
                ),
                3 => perm_resolved(&format!("never{}", rng.pick(4)), Some("o")),
                _ => turn_started(
                    if si == 0 { "s0" } else { "s1" },
                    &format!("t{}", rng.pick(2)),
                ),
            };
            apply_thread(&mut st, &ev);
            let ids = [session("s0"), session("s1")];
            for (i, sid) in ids.iter().enumerate() {
                if let Some(ts) = st.threads.get(sid) {
                    assert!(ts.messages.len() >= prev_len[i], "messages shrank");
                    prev_len[i] = ts.messages.len();
                }
            }
        }
        assert_thread_invariants(&st);
        drain_unrelated(ThreadsState::default());
    }

    #[test]
    fn t16_threads_state_serde_round_trip_byte_stable() {
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &created_ev("sb", "ag"));
        apply_thread(&mut st, &chunk_ev("sb", Role::User, "hi "));
        apply_thread(&mut st, &chunk_ev("sb", Role::User, "there"));
        apply_thread(
            &mut st,
            &upsert_ev("sb", "tc1", "ls", ToolCallStatus::Completed),
        );
        apply_thread(
            &mut st,
            &plan_ev(
                "sb",
                vec![PlanEntry {
                    content: "only".into(),
                    status: PlanEntryStatus::Completed,
                    priority: Some(crate::content::PlanPriority::High),
                }],
            ),
        );
        apply_thread(&mut st, &created_ev("sa", "ag")); // second session for key order
        apply_thread(&mut st, &perm_req("sa", "t", "rq"));

        let bytes = serde_json::to_string(&st).unwrap();
        // "sa" (smaller key) serializes before "sb" despite later arrival.
        assert!(bytes.find("\"sa\"").unwrap() < bytes.find("\"sb\"").unwrap());
        // Determinism via re-parse.
        let back: ThreadsState = serde_json::from_str(&bytes).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), bytes);
        assert_eq!(back, st);
    }

    #[test]
    fn t17_fuzz_all_nine_variants_totality_and_core_invariants() {
        let mut rng = Xorshift::new(42);
        let sessions = ["sa", "sb", "sc"];
        let statuses = [
            ToolCallStatus::Pending,
            ToolCallStatus::InProgress,
            ToolCallStatus::Completed,
            ToolCallStatus::Failed,
        ];
        let reasons = [
            StopReason::EndTurn,
            StopReason::MaxTokens,
            StopReason::MaxTurnRequests,
            StopReason::Refusal,
            StopReason::Cancelled,
        ];
        let mut st = ThreadsState::default();
        for step in 0..1200usize {
            let s = sessions[rng.pick(sessions.len())];
            let ev = match rng.pick(9) {
                0 => FxEvent::AgentStatus {
                    agent: agent("ax"),
                    driver: DriverId::CodexCli,
                    status: AgentStatus::Starting,
                },
                1 => created_ev(s, "ax"),
                2 => turn_started(s, &format!("t{}", rng.pick(3))),
                3 => chunk_ev(
                    s,
                    if rng.pick(2) == 0 {
                        Role::User
                    } else {
                        Role::Agent
                    },
                    "txt",
                ),
                4 => upsert_ev(
                    s,
                    &format!("tc{}", rng.pick(4)),
                    "card",
                    statuses[rng.pick(4)],
                ),
                5 => plan_ev(s, vec![]),
                6 => perm_req(
                    s,
                    &format!("tc{}", rng.pick(4)),
                    &format!("r{}", rng.pick(6)),
                ),
                7 => perm_resolved(&format!("r{}", rng.pick(6)), None),
                _ => FxEvent::TurnFinished {
                    session: session(s),
                    turn: turn(&format!("t{}", rng.pick(3))),
                    stop_reason: reasons[rng.pick(reasons.len())],
                },
            };
            apply_thread(&mut st, &ev); // totality: must never panic
            if step % 97 == 0 {
                assert_thread_invariants(&st);
            }
        }
        assert_thread_invariants(&st);
    }

    #[test]
    fn sequenced_inner_wiring_smoke() {
        // model folds take plain FxEvent refs; unwrap Sequenced at call sites.
        let seqd = Sequenced {
            seq: Seq::new(1),
            inner: turn_started("s", "t"),
        };
        let mut st = ThreadsState::default();
        apply_thread(&mut st, &seqd.inner);
        assert_eq!(thread(&st, &session("s")).active_turn, Some(turn("t")));
    }

    // ========================================================================
    // perms (apply_perms) — PermsState/ResolvedPermission arrive via `super::*`
    // (model's own flat re-exports); no separate import needed.
    // ========================================================================

    #[test]
    fn p1_request_inserts_into_pending() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_req("s", "tc", "r1"));
        assert!(st.pending.contains_key(&req("r1")));
    }

    #[test]
    fn p2_duplicate_request_overwrites_single_entry_latest_fields() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_req("s", "tc", "r1"));
        let latest = FxEvent::PermissionRequested {
            request_id: req("r1"),
            session: session("s2"),
            tool_call: ToolCallSummary {
                tool_call: tool("zz"),
                title: "new".into(),
            },
            options: vec![],
        };
        apply_perms(&mut st, &latest);
        assert_eq!(st.pending.len(), 1);
        let p = st.pending.get(&req("r1")).unwrap();
        assert_eq!(p.session, session("s2"));
        assert_eq!(p.summary.title, "new");
    }

    #[test]
    fn p3_resolution_removes_pending_and_appends_recent_with_choice() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_req("s", "tc", "r1"));
        apply_perms(&mut st, &perm_resolved("r1", Some("opt")));
        assert!(st.pending.is_empty());
        assert_eq!(st.recent.back().unwrap().chosen, Some(opt("opt")));
    }

    #[test]
    fn p4_none_choice_lands_as_cancelled_audit_row() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_req("s", "tc", "r1"));
        apply_perms(&mut st, &perm_resolved("r1", None));
        let row = st.recent.back().unwrap();
        assert_eq!(row.chosen, None);
        assert_eq!(row.request_id, req("r1"));
    }

    #[test]
    fn p5_unknown_resolution_still_audited() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_resolved("ghost", Some("o")));
        assert!(st.pending.is_empty());
        assert_eq!(st.recent.len(), 1);
        assert_eq!(st.recent[0].request_id, req("ghost"));
    }

    #[test]
    fn p6_recent_ring_is_capped_exactly_at_recent_cap() {
        let mut st = PermsState::default();
        for i in 0..RECENT_CAP + 10 {
            apply_perms(&mut st, &perm_resolved(&format!("r{i}"), None));
        }
        assert_eq!(st.recent.len(), RECENT_CAP);
        assert_eq!(
            st.recent.front().unwrap().request_id,
            req("r10"),
            "oldest 10 evicted"
        );
        assert_eq!(
            st.recent.back().unwrap().request_id,
            req("r59"),
            "newest retained"
        );
        // Strict resolution order preserved front→back.
        for (pos, row) in st.recent.iter().enumerate() {
            let want = 10 + pos;
            assert_eq!(row.request_id, req(&format!("r{want}")));
        }
    }

    #[test]
    fn p7_reapplied_resolution_yields_single_recent_entry() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_resolved("r1", Some("o")));
        apply_perms(&mut st, &perm_resolved("r1", Some("o")));
        assert_eq!(st.recent.len(), 1);
    }

    #[test]
    fn p8_non_permission_variants_leave_perms_state_untouched() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_req("s", "tc", "keep"));
        apply_perms(&mut st, &perm_resolved("swept_before", None));
        let before = st.clone();
        // The complements of {PermissionRequested, PermissionResolved}: agent
        // lifecycle + session/turn/transcript events.
        for ev in [
            status_ev("a", AgentStatus::Ready),
            created_ev("s", "a"),
            turn_started("s", "t"),
            chunk_ev("s", Role::User, "c"),
            upsert_ev("s", "tc", "title", ToolCallStatus::Pending),
            plan_ev("s", vec![]),
            finished("s", "t"),
        ] {
            apply_perms(&mut st, &ev);
            assert_eq!(st, before, "perms fold reacted to {ev:?}");
        }
    }

    #[test]
    fn p9_perms_state_round_trips_byte_stable() {
        let mut st = PermsState::default();
        apply_perms(&mut st, &perm_req("s", "tc", "r2"));
        apply_perms(&mut st, &perm_resolved("r2", Some("o")));
        apply_perms(&mut st, &perm_req("s", "tc2", "r1"));
        let bytes = serde_json::to_string(&st).unwrap();
        let back: PermsState = serde_json::from_str(&bytes).unwrap();
        assert_eq!(serde_json::to_string(&back).unwrap(), bytes);
        let expected_shape = ResolvedPermission {
            request_id: req("r2"),
            chosen: Some(opt("o")),
        };
        assert_eq!(back.recent[0], expected_shape);
    }
}
