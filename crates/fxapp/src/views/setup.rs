//! Agent setup screen (M3): DetectAgents results + guided enable/disable per driver.

use fxproto::driver::DriverId;
use fxproto::reply::DetectedDriver;

// TODO:
//
// pub struct SetupView {
//     detections: Vec<DetectedDriver>,   // filled by Reply::DetectedAgents
//     scanning: bool,
// }
//
// DATA SOURCE: LOCAL cache above — detections are command ANSWERS, not projected state:
// nothing about them belongs in AppState (folds only consume FxEvents, and there is no
// detection event). DetectedDriver is the CANONICAL wire type from fxproto reply.rs —
// never redefine a local shadow of it.
//
// INTENT → COMMAND: on open AND on "Re-scan" click ⇒ send(Command::DetectAgents) once
//   (re-click while scanning = ignored); stash rows in order of arrival, which is the
//   server's DriverId iteration order.
//
// RENDER per row ("setup-driver", driver):
//   label · found Badge + version string when Some ("claude-code-acp 1.2.3") ·
//   grey "not found" Badge when found:false (version is None by definition then — the
//   wire type guarantees it; render must not invent a version).
//   !found rows append the install hint as mono text — static table mirroring driver.rs
//   defaults: ClaudeCode → "npx -y @agentclientprotocol/claude-agent-acp" ·
//   GeminiCli → "gemini --acp" (needs gemini CLI) · CodexCli → "codex-acp".
//   If spec_used differs from that default (config override), show spec_used.program
//   instead and a "(configured)" suffix — the field exists exactly for this honesty.
//
// READ-ONLY STANCE (decided): no live toggles until a ConfigCommand exists on the wire
//   (M4+ protocol addition through fxproto goldens). Render per-driver checkboxes DISABLED
//   with tooltip "edit ~/.fxcode/config.toml to override", so users learn where the truth
//   lives instead of clicking dead UI.
//
// STATES ENUMERATED:
//   scanning  : skeleton rows — one placeholder per DriverId variant (3, constant).
//   results   : detections rendered as above.
//   error     : NONE — DetectAgents has no Error reply by pairing contract (command.rs);
//               worst case is all-rows-found:false, which is DATA.
//
// ElementIds additionally: "setup-rescan" button id.
