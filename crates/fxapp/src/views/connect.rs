//! Connect screen: server address + pairing token, first-run UX.

// TODO:
//
// pub struct ConnectScreen {
//     url_input: Entity<InputState>,      // default ws://127.0.0.1:PORT or last used
//     token_input: Entity<InputState>,
//     error: Option<String>,              // auth failed / version mismatch / unreachable
// }
//
// - "Connect" ⇒ persist to ClientState (cursor.rs) then ConnectionManager::spawn
// - status text driven by ConnStatus (Connecting attempt N…)
// - M3 polish: remember-me checkbox, QR/URL paste scheme later if mobile client happens
