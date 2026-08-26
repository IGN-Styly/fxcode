//! Thread view: one session's transcript + composer. The main screen.

// TODO:
//
// pub struct ThreadView {
//     active_session: Option<SessionId>,
//     composer: Entity<InputState>,        // gpui-component Input (stateful)
//     scroll: scroll handle → stick to bottom on new Chunk unless user scrolled up
// }
//
// Render from AppState.threads[active_session] (ThreadState.flow drives a VirtualList):
// - FlowItem::Message(idx)  → message::render(role, text)
// - FlowItem::Tool(id)      → tool_call::render(tool_calls[id])
// - plan section (collapsible) when plan non-empty
//
// Composer:
// - Enter ⇒ Command::Prompt { blocks: [Text] }; disable while active_turn.is_some()
// - stop button while turn active ⇒ Command::Cancel
// - empty state: "no session selected" placeholder
