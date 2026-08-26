//! Pairing token: our one application-level gate beyond Tailscale.
//!
//! This file is STORAGE + LIFECYCLE only. Verification (constant-time compare vs the
//! presented token) lives in net/handshake.rs via `subtle` — nothing here compares.
//!
//! No hashing at rest: file permissions ARE the control; handshake needs the plaintext
//! for constant-time comparison anyway — a hash would add ceremony, not security.
//!
//! Path semantics: NONE of the fns below hardcode ~/.fxcode — every caller passes
//! `cfg.data_dir` explicitly, which keeps --data-dir overrides and tempdir tests
//! honest. fxcore's Config docs pin <data_dir>/token as THE location.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rand::RngCore;
use rand::rngs::OsRng;

/// 32 random bytes => exactly 64 lowercase hex chars on disk.
const TOKEN_BYTES: usize = 32;
/// 64 hex chars + one trailing '\n' — that single line is the whole format.
const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Convenience bundle returned by [`ensure_token`] (path included so the first-boot
/// announcement can print where the token landed).
pub struct TokenFile {
    pub path: PathBuf,
    pub token: String,
}

// ── Public lifecycle ─────────────────────────────────────────────────────────

/// Token lifecycle:
///   1. create_dir_all(dir); on FRESH creation set mode 0o700 (create_dir_all cannot
///      set modes — chmod right after; pre-existing dirs are left as-is).
///   2. token file = <dir>/token:
///      - exists   => load_token(dir) (shape-validated; corrupt => Err, never
///        regenerate silently).
///      - missing  => generate + atomic write (see generate_token) AND print to
///        stderr EXACTLY ONCE per token's life — this branch only.
///   3. Return TokenFile { path, token }.
///
/// Every error propagates as io::Error; main.rs exits 1. Fail-closed everywhere:
/// see load_token for why corruption never auto-heals.
pub fn ensure_token(dir: &Path) -> io::Result<TokenFile> {
    let existed = dir.is_dir();
    std::fs::create_dir_all(dir)?;
    if !existed {
        set_mode(dir, 0o700)?;
    }
    let path = dir.join("token");
    if path.exists() {
        return Ok(TokenFile {
            token: load_token(dir)?,
            path,
        });
    }
    let token = generate_token(&path)?;
    announce(&token, &path);
    Ok(TokenFile { path, token })
}

/// Load + shape-validate <dir>/token.
///
/// Shape rule: 64 chars of [0-9a-f], optional trailing newline (\r\n tolerated).
/// Anything else => Err(InvalidData): FAIL CLOSED, NEVER regenerate here — silent
/// regeneration would sever every paired client with no warning; recovery is an
/// explicit --rotate-token (main.rs), which at least prints the new secret to
/// whoever runs the daemon. Unreadable/missing-mid-flight => io::Error up.
pub fn load_token(dir: &Path) -> io::Result<String> {
    let mut raw = String::new();
    File::open(dir.join("token"))?.read_to_string(&mut raw)?;
    // One trailing newline tolerated (\r\n also); anything beyond shape-check below.
    let body = raw
        .strip_suffix('\n')
        .map_or(&*raw, |s| s.strip_suffix('\r').unwrap_or(s));
    if body.len() != TOKEN_HEX_LEN || !body.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "pairing token at {} is corrupt (expected {TOKEN_HEX_LEN} lowercase hex \
                 chars) — recover with --rotate-token",
                dir.join("token").display()
            ),
        ));
    }
    Ok(body.to_owned())
}

/// Generate fresh token (same recipe + atomic write + 0o600), print it to stderr,
/// return it. Old clients fail their next handshake with close reason "auth_failed"
/// and must re-pair. Disruption window == the restart itself (main --rotate-token
/// exits before any listener binds).
pub fn rotate_token(dir: &Path) -> io::Result<String> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("token");
    let token = generate_token(&path)?;
    announce(&token, &path);
    Ok(token)
}

// ── Internals ────────────────────────────────────────────────────────────────

fn announce(token: &str, path: &Path) {
    eprintln!(
        "pairing token (shown once, also stored at {}): {token}",
        path.display()
    );
}

/// OsRng.fill(32B) → lowercase hex (`format!("{:02x}")` fold; no hex crate) → write
/// ATOMICALLY with privacy guaranteed at every instant:
///   create "<dir>/token.tmp" with mode 0o600 BEFORE writing → write → sync_all →
///   rename over <dir>/token. Rename is atomic within the dir and 0o600 rides on
///   the tmp file, so there is no moment where the token sits world-readable.
/// A failed write removes the tmp residue best-effort (tests pin this).
fn generate_token(path: &Path) -> io::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let tmp = path.with_extension("tmp");

    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
        file.set_permissions(PermissionsExt::from_mode(0o600))?;
        writeln!(file, "{token}")?;
        file.sync_all()
    })();
    match result {
        Ok(()) => {}
        Err(err) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
    }
    // std::io rename helper keeps semantics clear.
    std::fs::rename(&tmp, path)?;
    Ok(token)
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut perms = path.metadata()?.permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Tempdir scratch without a tempfile dep (mirrors fxcore/config.rs pattern):
    /// unique path per test, removed on drop. The dir is NOT pre-created —
    /// ensure_token/rotate_token must handle creation themselves (first-boot case).
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let dir = env::temp_dir().join(format!(
                "fxserver-pair-{tag}-{}-{nanos}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir); // stale-reuse belt & braces
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn token_path(&self) -> PathBuf {
            self.0.join("token")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mode_of(path: &Path) -> u32 {
        path.metadata().unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn first_boot_generates_persists_with_private_perms_and_fresh_dir_is_700() {
        let scratch = Scratch::new("first-boot");
        let tok = ensure_token(scratch.path()).unwrap();

        assert_eq!(tok.token.len(), TOKEN_HEX_LEN);
        assert!(
            tok.token
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
        assert_eq!(
            std::fs::read_to_string(scratch.token_path()).unwrap(),
            format!("{}\n", tok.token)
        );
        assert_eq!(
            mode_of(&scratch.token_path()),
            0o600,
            "token never world-readable"
        );
        assert_eq!(mode_of(scratch.path()), 0o700, "fresh data dir locked down");
    }

    #[test]
    fn second_boot_loads_identical_token() {
        let scratch = Scratch::new("second-boot");
        let first = ensure_token(scratch.path()).unwrap().token;
        let second = ensure_token(scratch.path()).unwrap().token;
        assert_eq!(first, second, "regeneration must be explicit rotation only");
    }

    #[test]
    fn corrupt_shapes_are_invalid_data_never_regen() {
        for (tag, contents) in [
            ("truncated", "abc123".to_owned()),
            ("nonhex", "Z".repeat(TOKEN_HEX_LEN)),
            ("uppercase", "ABCD".repeat(16)),
            ("oversized", "a".repeat(TOKEN_HEX_LEN + 1)),
            ("empty", String::new()),
        ] {
            let scratch = Scratch::new(tag);
            std::fs::create_dir_all(scratch.path()).unwrap();
            std::fs::write(scratch.token_path(), format!("{contents}\n")).unwrap();
            let err = load_token(scratch.path()).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{tag}");
            // ensure_token must NOT paper over corruption:
            assert!(ensure_token(scratch.path()).is_err(), "{tag}");
        }
        // \r\n tolerated; no-trailing-newline tolerated too.
        let scratch = Scratch::new("crlf");
        std::fs::create_dir_all(scratch.path()).unwrap();
        std::fs::write(scratch.token_path(), format!("{}\r\n", "b".repeat(64))).unwrap();
        assert_eq!(load_token(scratch.path()).unwrap(), "b".repeat(64));
        let scratch = Scratch::new("no-nl");
        std::fs::create_dir_all(scratch.path()).unwrap();
        std::fs::write(scratch.token_path(), "c".repeat(64)).unwrap();
        assert_eq!(load_token(scratch.path()).unwrap(), "c".repeat(64));
    }

    #[test]
    fn rotate_changes_token_keeps_perms_leaves_no_tmp() {
        let scratch = Scratch::new("rotate");
        let old = ensure_token(scratch.path()).unwrap().token;
        let new = rotate_token(scratch.path()).unwrap();
        assert_ne!(old, new, "rotation mints a genuinely fresh secret");
        assert_eq!(load_token(scratch.path()).unwrap(), new, "rotate persisted");
        assert_eq!(mode_of(&scratch.token_path()), 0o600);
        assert!(!scratch.token_path().with_extension("tmp").exists());
    }

    #[test]
    fn failed_generation_is_fail_closed_and_untouching() {
        // Read-only data dir: tmp creation itself fails; the error must propagate
        // and the PREVIOUS token must survive unharmed (no truncation attempt).
        let scratch = Scratch::new("failed-write");
        std::fs::create_dir_all(scratch.path()).unwrap();
        let keeper = "a".repeat(64);
        std::fs::write(scratch.token_path(), format!("{keeper}\n")).unwrap();

        let mut perms = scratch.path().metadata().unwrap().permissions();
        perms.set_mode(0o500); // r-x: no file creation inside
        std::fs::set_permissions(scratch.path(), perms).unwrap();

        assert!(rotate_token(scratch.path()).is_err());
        assert_eq!(
            load_token(scratch.path()).unwrap(),
            keeper,
            "old token intact"
        );

        // Restore so Drop cleanup can remove the scratch tree.
        let mut perms = scratch.path().metadata().unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(scratch.path(), perms).unwrap();
    }
}
