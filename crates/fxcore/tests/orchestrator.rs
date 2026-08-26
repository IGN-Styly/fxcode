//! Orchestrator integration tests against FakeAgent — the M1/M2 acceptance suite.

// TODO: scenarios (each = spawn Orchestrator w/ tempdir store + in-memory FakeAgent):
//
// happy_turn:
//   StartAgent → NewSession → Prompt → expect Chunk events in order → TurnFinished{EndTurn}
//   assert transcript fold reconstructs exact text.
//
// tool_call_lifecycle:
//   script tool_call(pending) then tool_call_update(completed) with same id
//   → ThreadsState.tool_calls has ONE entry, final status.
//
// permission_roundtrip:
//   script AskPermission mid-turn; drive Command::PermissionResponse from "client side";
//   turn completes only after answer. Also: Cancel while permission pending
//   → PermissionResolved{chosen: None} and agent received outcome=cancelled.
//
// crash_and_replay:
//   Crash mid-turn → AgentStatus::Crashed → restart orchestrator on SAME store dir
//   → projections rebuilt from log match pre-crash fold (golden compare).
//
// cursor_replay:
//   append N events via activity, subscribe from k < N → exactly N-k events replayed,
//   then live event flows through.
//
// ordering_guarantee:
//   hammer prompts concurrently; assert every subscriber sees strictly-increasing seq.
