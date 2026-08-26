//! Connect screen: server address + pairing token, first-run UX.

use crate::conn::{FatalError, ConnStatus};

// TODO:
//
// pub struct ConnectScreen {
//     url_input: Entity<InputState>,      // prefill: cursor::load().server_url
//                                         //   else "ws://127.0.0.1:8949" (DEFAULT_PORT twin,
//                                         //   keep in lockstep with fxserver ifaddr.rs)
//     token_input: Entity<InputState>,    // prefill: cursor::load().token (echo-off Input)
//     error: Option<String>,              // the ONE error line; mapping below
// }
//
// DATA SOURCE: local only — cursor.rs ClientState + ws.rs normalize_url. No AppState.
//
// INTENT → ACTIONS on "Connect" click (id "connect-submit"), strictly in order:
//   1. conn::ws::normalize_url(url_input) — Err ⇒ set error line from UrlError variant,
//      STOP before any dial or persistence (never remember a URL that failed validation).
//   2. persist ClientState { server_url, token, last_seq untouched } via cursor::save();
//      Err ⇒ error line "could not save client state" + stop (disk problems are user-
//      visible here rather than silently dropped credentials).
//   3. ConnectionManager::spawn(cx, url, token) — replaces any prior entity (exactly one
//      manager per process); status transitions render below.
//
// STATUS / ERROR LINE MAPPING (exact strings; close-string → human text):
//   Connecting { attempt }        ⇒ "Connecting (attempt N)…" — form dims but stays
//                                    editable so a wrong token can be fixed mid-retry;
//                                    clicking Connect again restarts the manager cleanly.
//   Disconnected { fatal: None }  ⇒ "Not connected."
//   Dial/transport failures       ⇒ "Could not reach <url> — retrying automatically"
//                                    (transient line while attempts continue).
//   FatalError::AuthFailed        ⇒ "Pairing token rejected. Paste the token fxserver
//                                    printed on first boot."        ← "auth_failed"
//   FatalError::ProtocolVersion   ⇒ "Protocol version mismatch — update fxapp and
//                                    fxserver together."             ← "protocol_version"
//   Fatal states PARK the reconnect loop (conn/mod.rs), so the submit button must be
//   re-enabled and highlighted as the way forward.
//
// ElementIds: "connect-url" · "connect-token" · "connect-submit" · "connect-error".
//
// STATES ENUMERATED: initial(prefilled) · validating(inline error, no dial yet) ·
//   dialing(dimmed inputs + attempt counter) · fatal(red error + enabled submit).
//
// M3 polish (unchanged): remember-me checkbox, QR/URL paste scheme if mobile happens.
