//! Resume cursor: how reconnects reconnect as replay instead of resync.
//!
//! Persisted shape (`~/.fxcode/client-state.json`, file name in
//! [`STATE_FILE_NAME`]): the server url + pairing token chosen on the connect
//! screen and the seq of the last event actually folded into the stores. The
//! FILE stores a bare JSON object whose `last_seq` is a plain u64 — fxproto's
//! ids::Seq is #[serde(transparent)] so the bytes are identical anyway;
//! conversion happens at these exact seams and nowhere else in fxapp:
//!
//!   ingest:    ev.seq.as_u64()                 → ClientState.last_seq
//!   subscribe: Seq::from_raw(self.last_seq)    → envelope Message::Subscribe { .. }
//!   snapshot:  snapshot.baseline_seq.as_u64()  → ClientState.last_seq

use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The one persisted client-side artifact. Also carries the connect-screen
/// memory (server url + token); the theme key is reserved for later — do not
/// add fields ad hoc, extend the JSON writer/reader together with this doc.
///
/// NOTE (serde substitution): fxapp cannot derive Serialize/Deserialize here —
/// `serde` itself is not a direct dependency of this crate (deps are fixed) —
/// so the JSON mapping below is written by hand against serde_json only.
/// Keep field names byte-compatible with what a `#[derive(Deserialize)]`
/// version would produce: `server_url`, `token`, `last_seq`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientState {
    pub server_url: Option<String>,
    /// Plaintext ok? yes: file is chmod 600; identical trust to the server's
    /// own plaintext-at-rest token (fxcore pair.rs "no hashing" rationale).
    pub token: Option<String>,
    pub last_seq: u64,
}

/// Fixed file name inside [`default_dir()`]/the injected dir.
pub const STATE_FILE_NAME: &str = "client-state.json";

/// chmod applied when WE created the directory (pre-existing dirs are left
/// alone — pair.rs precedent; create_dir_all cannot set modes).
const DIR_MODE: u32 = 0o700;
/// chmod set ON CREATION of the tmp file: there is no instant where state
/// sits world-readable.
const FILE_MODE: u32 = 0o600;

/// Where state lives by default. Tests inject their own dir via
/// [`load`]/[`save`] instead of using this — that is why they exist.
pub fn default_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(".fxcode"),
        _ => {
            tracing::warn!("HOME is unset; falling back to ./fxcode for client state");
            PathBuf::from(".fxcode")
        }
    }
}

fn path_in(dir: &Path) -> PathBuf {
    dir.join(STATE_FILE_NAME)
}

/// Load the cursor from `dir`. Missing file ⇒ Default silently (first run is
/// normal). Corrupt JSON / wrong shape ⇒ Default + warn. NEVER attempt partial
/// salvage: a lost cursor costs one bounded replay or one SnapshotRequired on
/// next boot.
pub fn load(dir: &Path) -> ClientState {
    let bytes = match std::fs::read(path_in(dir)) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return ClientState::default(),
        Err(err) => {
            tracing::warn!(error = %err, path = %path_in(dir).display(), "could not read client state");
            return ClientState::default();
        }
    };

    match parse(&bytes) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(error = ?error, "client state is corrupt; starting from defaults");
            ClientState::default()
        }
    }
}

fn parse(bytes: &[u8]) -> Result<ClientState, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let map = match value.as_object() {
        Some(map) => map,
        None => {
            // Wrong shape entirely — same treatment as corrupt JSON.
            return Ok(ClientState::default());
        }
    };
    let state = ClientState {
        server_url: map.get("server_url").and_then(as_owned_string),
        token: map.get("token").and_then(as_owned_string),
        last_seq: map
            .get("last_seq")
            .and_then(|v| v.as_u64())
            .unwrap_or_default(),
    };
    Ok(state)
}

fn as_owned_string(value: &serde_json::Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

impl ClientState {
    /// ATOMIC-WRITE RECIPE (exactly):
    ///   1. create_dir_all(dir); if that CREATED it, chmod 0o700 right after.
    ///   2. pretty JSON → tmp file opened mode 0o600 + sync_all (durable tmp).
    ///   3. rename(tmp ⇒ client-state.json) — atomic within the directory;
    ///      readers see either the old or new file whole, never torn writes.
    ///   4. Any step Err ⇒ best-effort remove_file(tmp), return the Err. The
    ///      PREVIOUS file stays valid: save failure never corrupts boot state.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let created_it = !dir.exists();
        std::fs::create_dir_all(dir)?;
        if created_it {
            // We made it: tighten owner bits right after (pair.rs precedent —
            // pre-existing directories are left alone).
            if let Err(error) =
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))
            {
                tracing::warn!(error = %error, "could not chmod client-state directory");
            }
        }

        let json = serde_json::json!({
            "server_url": self.server_url.as_deref(),
            "token": self.token.as_deref(),
            "last_seq": self.last_seq,
        });
        let body = serde_json::to_string_pretty(&json)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))?;

        let final_path = path_in(dir);
        let tmp_path = dir.join(format!("{STATE_FILE_NAME}.tmp"));
        let result = (|| {
            let mut file = OpenOptions::new()
                .mode(FILE_MODE)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            file.write_all(body.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&tmp_path, &final_path)?;
            Ok(())
        })();

        if result.is_err() {
            _ = std::fs::remove_file(&tmp_path);
        }
        result
    }

    /// Advance helper for the ingest seam: fold FIRST (store/mod.rs apply),
    /// THEN this — never the reverse. An over-cursor save can permanently skip
    /// unapplied events; under-cursor saves merely replay forward safely.
    pub fn advance_to_seq(&mut self, seq: u64) {
        self.last_seq = seq;
    }
}

// TIMING RULES (conn/mod.rs enforces the order, this file owns why):
//   - Per live/replayed event: fold FIRST, then last_seq = ev.seq, THEN
//     save(). Crash between fold and save ⇒ next boot replays ≥ cursor; folds
//     tolerate re-delivery of keyed events and replay windows stay tiny.
//   - NEVER save(last_seq = seq) BEFORE the fold for that seq has run — that
//     is the one unrecoverable bug here ([ClientState::advance_to_seq] documents
//     the seam so callers can't get it backwards).
//   - Replay bursts use the same per-event cadence; simplicity beats batching
//     until profiling says otherwise.
//   - SnapshotRequired path: stores REPLACED first, last_seq = baseline_seq,
//     ONE save.

#[cfg(test)]
mod tests {
    use super::*;

    /// No tempfile dep allowed; scope name per test by caller suffix.
    fn tempdir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fxapp-cursor-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn sample() -> ClientState {
        ClientState {
            server_url: Some("ws://127.0.0.1:8949/ws".into()),
            token: Some("pair-token".into()),
            last_seq: 42,
        }
    }

    #[test]
    fn save_then_load_round_trips_through_tempdir() {
        let dir = tempdir("roundtrip");
        let state = sample();
        state.save(&dir).expect("save");
        assert_eq!(load(&dir), state);
    }

    #[test]
    fn missing_file_loads_default_silently() {
        let dir = tempdir("missing");
        assert_eq!(load(&dir), ClientState::default());
    }

    #[test]
    fn corrupt_json_falls_back_to_default() {
        let dir = tempdir("corrupt");
        std::fs::write(path_in(&dir), b"{ not really json").unwrap();
        assert_eq!(load(&dir), ClientState::default());
    }

    #[test]
    fn wrong_shape_is_treated_like_corruption() {
        let dir = tempdir("shape");
        std::fs::write(path_in(&dir), br#"[1, 2, 3]"#).unwrap();
        assert_eq!(load(&dir), ClientState::default());
    }

    #[test]
    fn partial_fields_survive_with_defaults_for_the_rest() {
        let dir = tempdir("partial");
        std::fs::write(
            path_in(&dir),
            br#"{"last_seq": 9, "server_url": null, "nope": true}"#,
        )
        .unwrap();
        assert_eq!(
            load(&dir),
            ClientState {
                server_url: None,
                token: None,
                last_seq: 9
            }
        );
    }

    #[test]
    fn state_file_and_dir_permissions_are_tightened() {
        // The save target must NOT exist beforehand: chmod-700 applies only when
        // cursor::save created the directory itself (pair.rs precedent).
        let dir = tempdir("perms").join("leaf");
        sample().save(&dir).expect("save");
        let file_mode = std::fs::metadata(path_in(&dir))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(file_mode & 0o777, FILE_MODE);
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, DIR_MODE);
    }

    #[test]
    fn no_torn_writes_final_file_never_left_as_tmp() {
        let dir = tempdir("torn");
        sample().save(&dir).expect("save");
        assert!(path_in(&dir).exists());
        assert!(!dir.join(format!("{STATE_FILE_NAME}.tmp")).exists());
    }

    #[test]
    fn advance_only_moves_forward_via_explicit_seam() {
        let mut st = ClientState::default();
        st.advance_to_seq(u64::MAX);
        assert_eq!(st.last_seq, u64::MAX);
    }
}
