//! Handshake + subscription replay. The security boundary lives here.

// Imports to restore as you implement:
// use crate::pair;              // token source
// use fxcore::store::EventStore;
// use fxproto::envelope::{Message, PROTO_VERSION};

// TODO: flow (all before ANY command is honored):
//
//   await first frame = Message::Hello { proto_version, token }
//     - proto_version != PROTO_VERSION ⇒ close with code + reason (no fallback negotiation)
//     - token compare vs load_token(): CONSTANT TIME via subtle::ConstantTimeEq
//     - failures get a close frame with a distinct reason string (client UX), then socket dies
//   await Message::Subscribe { last_seq }
//     - gap policy: head - last_seq > N (pick ~10k) ⇒ send SnapshotRequired{baseline, snapshot}
//       else stream store.replay(last_seq) as Event frames, THEN attach live bus
//     - ordering guarantee: replay fully drained before first live event forwarded
//       (buffer live events during replay; seq-check to dedupe overlap)
//
// After handshake succeeds, hand off to client::run with an AuthedClient context.
//
// TODO tests: wrong version / wrong token / replay-then-live / snapshot path.
