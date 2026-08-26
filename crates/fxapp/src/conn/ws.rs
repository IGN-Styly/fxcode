//! THE ONLY FILE THAT KNOWS TOKIO EXISTS (docs/crates.md rule).
//!
//! Owns a small embedded tokio Runtime running async-tungstenite; bridges frames to
//! GPUI's executor via channels. If GPUI-native async ever suffices, only this file dies.

use futures::{SinkExt, StreamExt};

// Imports to restore as you implement (the live `use` above stays):
// use std::sync::{Arc, OnceLock};
//
// use serde_json; // via fxproto::envelope::Message Serialize/Deserialize
// use tokio::runtime::Runtime;
// use tokio::time::interval;
//
// use fxproto::envelope::Message;

// FLAG (Cargo.toml — outside this scaffold's edit scope): add `flume = "0.11"` to
// crates/fxapp/Cargo.toml + [workspace.dependencies]. Nothing else new here.

// TODO: channel flavor DECIDED — FLUME, bounded. One-line why: flume receivers await
// natively on ANY executor, so GPUI's smol-based side does zero-copy async recv while the
// quarantined tokio runtime pushes with plain sync sends — the std-mpsc-plus-GPUI-timer
// alternative would inject timer-tick latency into every streamed chunk and burn UI-thread
// polls. (Same reasoning makes bounded-flume-as-oneshot right for conn/mod.rs replies.)
//
// pub enum Frame { Out(fxproto::envelope::Message), In(...) }  // NO — both directions are
//                                                              // just envelope::Message.
//
// Channel inventory (flume, BOTH BOUNDED):
//   out_tx: flume::bounded::<Message>(16)   GPUI → runtime. Commands are human-paced
//                                           (Prompt/Cancel/PermissionResponse); 16 slots
//                                           ≫ burst need. try_send Full ⇒ the link is
//                                           effectively unusable ⇒ surface Err up to
//                                           send() as Transport rather than queue blindly.
//   in_rx:  flume::bounded::<Message>(1024) runtime → GPUI. Capacity MATCHES the fxcore
//                                           bus cap (fxcore/src/bus.rs ~1024) so this
//                                           bridge is never the artificial bottleneck;
//                                           in_rx.send().await backpressures tungstenite's
//                                           reads → TCP backs up → fxserver's out-Full rule
//                                           kicks us with Close("resubscribe"). Coherent.
//
// pub struct WsHandle {
//     out_tx: flume::Sender<Message>,      // main → runtime task
//     in_rx: flume::Receiver<Message>,     // runtime task → main
//     rtt_ms: Arc<AtomicU64>? watch-cell,  // latest ping RTT for the M0 latency badge
//                                          // (status bar reads it; conn/mod.rs never does)
//     broken: Arc<AtomicBool>,             // set by pumps on fatal socket error
// }
// Dropping WsHandle drops the channels ⇒ pump tasks see disconnect and close the WS.
// No explicit close() API needed for v0.
//
// impl WsHandle {
//     /// Connect + return handle. DNS/TCP/WS-upgrade errors surface as Err HERE;
//     /// protocol-level auth/version failures arrive as an inbound WS Close frame
//     /// carrying the canonized reason string (envelope.rs) via in_rx consumption —
//     /// classification is CONN/MOD.RS's job, not this file's.
//     pub fn connect(url: &str) -> Result<Self>;
// }
//
// RUNTIME LIFECYCLE: ONE lazily-initialized Runtime
// (tokio::runtime::Builder::new_multi_thread().enable_all(), stored in a OnceLock,
// built on first connect) for the whole process. Reconnects build fresh WsHandles over
// the SAME runtime — never per-connection runtimes.
//
// Pump tasks (spawned onto that runtime per WsHandle; three small ones):
//   WRITE: out_rx.recv_async() → serde_json::to_string → sink.send(Text). Encode/socket
//          error ⇒ set broken, exit (reader notices soon; conn/mod.rs sees EOF).
//   READ : stream.next() → serde_json::from_str::<Message> ⇒ in_rx.send(msg).await.
//          UNDECODABLE frame from our own server is an integration bug, not client UX:
//          tracing::error! + treat as fatal socket error (set broken, exit). Server-initiated
//          Pings are answered by tungstenite automatically — no code here.
//   KEEPALIVE: tokio interval every 20_000 ms ⇒ sink.send(Ping); match arriving Pongs to
//          sent times, publish rtt_ms. DEAD-PEER RULE: no inbound frame OF ANY KIND
//          (Pong included) for 60_000 ms (= 3 intervals) ⇒ set broken, exit all pumps ⇒
//          conn/mod.rs observes silence via its read loop ending / WsHandle.broken.
//          Numbers rationale: 20s keeps NAT/tailscale mappings warm at negligible cost;
//          60s tolerates 2 lost probes before declaring death.
//
// TODO: normalize_url(input: &str) -> Result<Url, UrlError> — runs BEFORE dialing;
// ConnectScreen renders the Err message verbatim. Implementation-ready rules:
//   - scheme MUST be exactly `ws` or `wss` (ASCII case-insensitive). Anything else —
//     missing scheme ("localhost:8949"), http/https, other schemes — is Err, NEVER guessed
//     or auto-prepended: silently defaulting the scheme risks pointing a pairing token at
//     a plaintext endpoint the user did not type.
//   - host REQUIRED (IPv4/IPv6/hostname; empty ⇒ Err).
//   - port OPTIONAL: absent ⇒ substitute DEFAULT_PORT = 8949 — MUST be kept in lockstep
//     with fxserver ifaddr::DEFAULT_PORT (golden port; single drift risk, noted in both).
//   - path: "" or "/" normalized to "/ws" (the one route fxserver serves, net/mod.rs);
//     any other path, any query `?...`, any fragment `#...` ⇒ Err(UnknownPath/BadParts).
//   - Error enum: UnsupportedScheme { got: String }, MissingHost, BadPath { path: String }.
//
// TODO tests: normalize_url table (every rule above has one accept + one reject row);
// keepalive unit: fake clock advances 61s ⇒ broken flips true.
