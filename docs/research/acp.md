# ACP — the Agent Client Protocol

Research notes, verified against primary sources (Aug 2026). Audience: TS/Next.js devs new to Rust/desktop.

## What it is

ACP is an open protocol (started by Zed Industries, now governed under the
`agentclientprotocol` GitHub org) that standardizes how a **client** — usually a code editor — talks to
**coding agents** like Claude Code or Gemini CLI. It's explicitly pitched as "the LSP for agents": instead
of every editor integrating every agent by hand, agents implement ACP once and work in any ACP client
(agentclientprotocol.com/get-started/introduction). The client **spawns the agent as a subprocess** on demand
and communicates over its stdin/stdout; one connection can host multiple concurrent sessions
(agentclientprotocol.com/get-started/architecture). Note: Zed itself is written in Rust, but as a client
implementer you never touch Rust — there's a first-party TypeScript SDK.

## Transport & wire format

JSON-RPC 2.0 over stdio (agentclientprotocol.com/protocol/v1/transports):

- Client launches agent as subprocess; requests go to its `stdin`, responses/notifications come from `stdout`.
- Messages are newline-delimited JSON (`\n`, no embedded newlines), UTF-8.
- Agent may log freely to `stderr`; it must **not** write non-ACP bytes to `stdout`.
- Remote transports (HTTP/WebSocket) are draft RFDs, not stable yet. Custom transports allowed.

Think of each line as a JSON-RPC message — same shapes you'd get from any JSON-RPC lib: `request`
(`id` + `method` + `params`), `response` (`id` + `result`), and `notification` (no `id`).

## Core lifecycle

1. **`initialize`** — client sends its latest supported `protocolVersion` (integer; current stable = 1) plus
   `clientCapabilities` (e.g. `fs.readTextFile/writeTextFile`, `terminal`). Agent replies with the negotiated
   version, `agentCapabilities` (e.g. `loadSession`, prompt content types, MCP transports), and `authMethods`
   (agentclientprotocol.com/protocol/v1/initialization).
2. **`session/new`** — client passes `{ cwd, mcpServers }`; agent returns a `sessionId`. The `cwd` anchors the
   session's filesystem scope; `mcpServers` lets the client hand the agent tool servers to connect to
   (agentclientprotocol.com/protocol/v1/session-setup). Optional variants: `session/load` (replay full history),
   `session/resume`, `session/close`, `session/delete` — all capability-gated.
3. **`session/prompt`** — send user content blocks (text, images, embedded resources); the request resolves only
   when the whole turn finishes, with a `stopReason`: `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`,
   or `cancelled`. Cancel mid-turn via the `session/cancel` notification
   (agentclientprotocol.com/protocol/v1/prompt-turn).

React analogy: a session ≈ one chat thread's server state; `sessionId` ≈ thread id you attach to every mutation.

## Streaming updates

While a turn runs, the agent streams **`session/update` notifications** whose `update.sessionUpdate` field
discriminates the kind (full list from schema/v1/schema.json `$defs.SessionUpdate`):
`user_message_chunk`, `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`,
`plan`, `available_commands_update`, `current_mode_update`, `config_option_update`,
`session_info_update`, `usage_update`.

Example exchange (verbatim from agentclientprotocol.com/protocol/v1/prompt-turn):

```json
{"jsonrpc":"2.0","id":2,"method":"session/prompt",
 "params":{"sessionId":"sess_abc123def456","prompt":[{"type":"text","text":"Can you analyze this code?"}]}}

{"jsonrpc":"2.0","method":"session/update",
 "params":{"sessionId":"sess_abc123def456","update":{"sessionUpdate":"agent_message_chunk",
   "content":{"type":"text","text":"I'll analyze your code for potential issues..."}}}}

{"jsonrpc":"2.0","method":"session/update",
 "params":{"sessionId":"sess_abc123def456","update":{"sessionUpdate":"tool_call",
   "toolCallId":"call_001","title":"Analyzing Python code","kind":"other","status":"pending"}}}

// later, same toolCallId:
{"jsonrpc":"2.0","method":"session/update",
 "params":{"sessionId":"sess_abc123def456","update":{"sessionUpdate":"tool_call_update",
   "toolCallId":"call_001","status":"completed"}}}

{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}
```

Tool calls arrive once (`tool_call`, status `pending`) then mutate in place via `tool_call_update`
(`in_progress` → `completed`). In React terms: notifications are perfect for `useReducer`-style appends,
and `toolCallId` is your natural key for upserting into a map.

## Permissions model

Agents don't just run tools — they ask first. `session/request_permission` is a **request from agent back to
client** (JSON-RPC is bidirectional here), carrying the pending `toolCall` details and an array of options
(schema/v1/schema.json; agentclientprotocol.com/protocol/v1/prompt-turn):

```json
{"jsonrpc":"2.0","id":5,"method":"session/request_permission",
 "params":{"sessionId":"...","toolCall":{...},
   "options":[{"optionId":"allow","name":"Allow","kind":"allow_once"},
              {"optionId":"deny","name":"Deny","kind":"reject_once"}]}}
```

Each option has `kind`: `allow_once`, `allow_always`, `reject_once`, `reject_always`. The client responds with
`{"outcome":{"outcome":"selected","optionId":"allow"}}` — or `"cancelled"` if the user aborted the turn
(clients MUST then answer pending permission requests with that outcome).

## Auth methods

The `initialize` response advertises `authMethods`; the client calls `authenticate { methodId }`
(agentclientprotocol.com/protocol/v1/authentication). Two flows:

- `type: "agent"` (default): the agent performs login itself during `authenticate` — e.g. Gemini CLI
  advertises "Log in with Google" and drives the OAuth flow internally
  (gemini-cli `packages/cli/src/acp/acpRpcDispatcher.ts`).
- `type: "terminal"`: the client re-launches the agent binary interactively in a terminal so the user logs
  in there, then reconnects.

An optional `logout` method exists, gated behind `agentCapabilities.auth.logout`.

## Ecosystem (today)

- **Clients**: Zed (native), JetBrains IDEs, Qt Creator, Emacs/neovim/Obsidian plugins, VS Code extensions,
  plus many standalone desktop/web/mobile/messaging apps (agentclientprotocol.com/get-started/clients).
- **Agents**: Gemini CLI natively (`gemini --acp`); Claude Code via the official adapter npm package
  `@agentclientprotocol/claude-agent-acp` (renamed from `@zed-industries/claude-code-acp`, which is now
  deprecated); Codex CLI via Zed's `codex-acp` adapter; also OpenCode, Goose, Qwen Code, Cursor CLI,
  Copilot CLI (preview), Cline and dozens more (agentclientprotocol.com/get-started/agents).
- **SDKs**: official TypeScript (`@agentclientprotocol/sdk`), Rust, Python, Kotlin, Java — TS and Rust at 1.0
  (github.com/agentclientprotocol/agent-client-protocol README).
- Protocol v1 is stable; a v2 draft exists with breaking changes (docs split at /protocol/v1 vs /protocol/v2).

## Why an "agent manager" GUI should care

You want to drive Claude Code, Gemini CLI, etc. from your own UI without embedding each vendor's SDK.
With ACP you implement **one client**: spawn any compliant agent binary, speak ndjson over stdio, and you get
for free the hard parts of agentic UX — streaming chunks, live plan/tool-call state, permission prompts, auth
flows, session resume/load. Your Next.js frontend maps `session/update` notifications straight onto React
state; the only "native" part is spawning a child process (trivial in Node/Electron/Tauri). No Rust required.

## Sources

- https://agentclientprotocol.com/get-started/introduction
- https://agentclientprotocol.com/get-started/architecture
- https://agentclientprotocol.com/protocol/v1/transports
- https://agentclientprotocol.com/protocol/v1/initialization
- https://agentclientprotocol.com/protocol/v1/session-setup
- https://agentclientprotocol.com/protocol/v1/prompt-turn
- https://agentclientprotocol.com/protocol/v1/authentication
- https://github.com/agentclientprotocol/agent-client-protocol (README; schema/v1/schema.json)
- https://www.npmjs.com/package/@zed-industries/claude-code-acp (deprecation notice → rename)
- https://github.com/google-gemini/gemini-cli (docs/cli/acp-mode.md; packages/cli/src/acp/*)
