//! Agent setup screen (M3): DetectAgents results + guided enable/disable per driver.

// TODO:
//
// pub struct SetupView { detections: Vec<DetectedDriver> }
//
// - on open ⇒ Command::DetectAgents; render found/not-found + versions
// - not-found rows show the install hint (npm/npx/brew command per driver)
// - enable/disable toggles write to... DECISION: client edits are server-side config;
//   either send a future ConfigCommand through the protocol or instruct user to edit
//   ~/.fxcode/config.toml. Lean protocol command in M4+; read-only view until then.
