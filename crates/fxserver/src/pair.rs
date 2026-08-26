//! Pairing token: our one application-level gate beyond Tailscale.

// Imports to restore as you implement:
// use std::path::{Path, PathBuf};

// NOTE: fxcore owns Config; fxserver passes cfg.data_dir into these fns.

// TODO:
//
// /// Token lifecycle:
// /// - ensure_token(dir): read ~/.fxcode/token if exists; else generate
// ///   (32 random bytes, hex via rand), write chmod 600, print once to stderr:
// ///     "pairing token (shown once, also stored at ~/.fxcode/token): <token>"
// /// - load_token(dir) -> String          // for handshake verification
// /// - rotate_token(dir) -> String        // regenerate + print; invalidates old clients
// ///
// /// Storage format: single line, hex, trailing newline tolerated on read.
// /// No hashing needed at rest (file perms are the control); constant-time COMPARE
// /// happens in net/handshake.rs using `subtle`.
// pub fn ensure_token(dir: &Path) -> std::io::Result<TokenFile>;
// pub fn load_token(dir: &Path) -> std::io::Result<String>;
// pub fn rotate_token(dir: &Path) -> std::io::Result<String>;
//
// pub struct TokenFile { pub path: PathBuf, pub token: String }  // convenience return
