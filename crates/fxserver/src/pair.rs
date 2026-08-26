//! Pairing token: our one application-level gate beyond Tailscale.
//!
//! This file is STORAGE + LIFECYCLE only. Verification (constant-time compare vs the
//! presented token) lives in net/handshake.rs via `subtle` — nothing here compares.

// Imports to restore as you implement:
// use std::fs::{File, OpenOptions};
// use std::io::{Read, Write};
// use std::os::unix::fs::PermissionsExt;   // POSIX-only crate rule: no Windows paths
// use std::path::{Path, PathBuf};
//
// use rand::rngs::OsRng;
// use rand::RngCore;

// NOTE: fxcore owns Config; fxserver passes cfg.data_dir into every fn below — none of
// them hardcode ~/.fxcode (keeps --data-dir overrides and tempdir tests honest).

// TODO:
//
// /// Constants: TOKEN_BYTES = 32; token file = <dir>/token holding EXACTLY 64
// /// lowercase hex chars + one trailing '\n'. That single line is the whole format.
//
// pub struct TokenFile { pub path: PathBuf, pub token: String }  // convenience return
//
// /// Token lifecycle:
// pub fn ensure_token(dir: &Path) -> std::io::Result<TokenFile>
//   1. create_dir_all(dir); on fresh creation set mode 0o700 (create_dir_all cannot
//      set modes — chmod right after; pre-existing dirs are left as-is).
//   2. path = dir.join("token").
//      - File exists  => load_token(dir) (shape-validated; corrupt => Err, never
//        regenerate silently — see load_token).
//      - File missing => generate: OsRng.fill(&mut [0u8; 32]), hex-encode lowercase
//        (format!("{:02x}") fold; do NOT add a hex crate for this). Write ATOMICALLY:
//        create "<dir>/token.tmp" with PermissionsExt mode 0o600 BEFORE writing =>
//        write + sync_all => rename over <dir>/token. Rename is atomic within the
//        dir, and the 0o600 rides on the tmp file, so there is no instant where the
//        token sits world-readable.
//      - PRINT to stderr ONLY in this generate branch, exactly once per token's life:
//          "pairing token (shown once, also stored at <path>): <token>"
//   3. Return TokenFile { path, token }.
//
// pub fn load_token(dir: &Path) -> std::io::Result<String>
//   Read <dir>/token, strip trailing newline (\n; tolerate \r\n). VALIDATE SHAPE:
//   exactly 64 chars, all [0-9a-f]. Anything else => Err(InvalidData): FAIL CLOSED,
//   NEVER regenerate here — silent regeneration would sever every paired client with
//   no warning; recovery is an explicit --rotate-token (main.rs step 4), which at
//   least prints the new secret to whoever runs the daemon.
//   Unreadable/missing-mid-flight => propagate the io::Error; main exits 1.
//
// pub fn rotate_token(dir: &Path) -> std::io::Result<String>
//   Generate fresh token (same recipe + atomic write + 0o600), print it to stderr,
//   return it. Old clients fail their next handshake with close reason "auth_failed"
//   and must re-pair. Disruption window == the restart itself (main.rs --rotate-token
//   exits before binding).
//
// Error behavior summary (all fns):
//   | condition                          | result                          |
//   |------------------------------------|---------------------------------|
//   | dir creation fails (EACCES etc.)    | io::Error up; main exits 1      |
//   | token file unreadable               | io::Error up; main exits 1      |
//   | token file corrupt (bad shape)      | Err(InvalidData); main exits 1  |
//   No path regenerates without human-visible stderr output.
//
// No hashing at rest: file permissions ARE the control; handshake needs the plaintext
// for constant-time comparison anyway — a hash would add ceremony, not security.
//
// TODO tests (tempdir-backed):
//   first boot generates + prints + persists; second boot loads identical token;
//   truncated / non-hex / oversized file => InvalidData; rotate changes the token;
//   failed write leaves no .tmp behind.
