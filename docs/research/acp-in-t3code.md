# T3 Code and its use of ACP

Researched 2026-08-26. All claims below were verified against primary sources (official repo, official site, and the actual source code, cloned at commit `a3a8cbd`, 2026-08-26).

## 1. What T3 Code is

T3 Code ([github.com/pingdotgg/t3code](https://github.com/pingdotgg/t3code), [t3.codes](https://t3.codes)) is an open-source **"agent harness control surface"** — a GUI control plane for orchestrating multiple coding-agent CLIs on your machine. It is published by **T3 Tools Inc.** under Theo Browne's `pingdotgg` GitHub org (the t3.gg / T3 Stack ecosystem; no fetched page names Theo personally, but the branding is unambiguous).

**Stack — NOT GPUI/Rust.** Despite community assumption, it is a **TypeScript monorepo**:

- **Electron desktop app** + web app (`app.t3.codes`) + iOS/Android mobile clients
- pnpm workspaces + "Vite+" toolchain, heavy use of **Effect TS**
- MIT licensed (~20.6k stars; README/site say "fork the whole thing")

Supported agents ("harnesses"), bring-your-own-subscription: **Claude Code, Codex CLI, Cursor CLI, Grok Build CLI, OpenCode**.

## 2. Architecture (from `docs/internals/`)

A deliberate client/server split:

```
Clients (web/desktop/mobile) ──Effect RPC over WebSocket──▶ Server ──per-driver transport──▶ Agent CLIs
```

- The **server owns everything**: agent processes, terminals, git, filesystem. Clients are thin.
- **Event-sourced orchestration engine**: typed commands → persisted events → projections into a read model (decider/projector pattern). Totally ordered command queue, idempotent retries.
- **Provider driver registry**: 5 built-in drivers (`codex`, `claudeAgent`, `cursor`, `grok`, `opencode`). Each driver declares config schema and builds an adapter behind a common `ProviderAdapter` contract, so orchestration code never knows which agent is behind a thread.
- **Checkpointing per turn** via hidden Git refs; diffs/reverts computed between checkpoints. Threads can run in isolated Git worktrees.

## 3. How it actually speaks ACP

ACP usage is real but **partial — 2 of 5 drivers speak ACP natively** (verified in source):

| Driver | Transport | ACP? |
| --- | --- | --- |
| Cursor | spawns `cursor-agent acp` (stdio JSON-RPC) | **Yes** |
| Grok | spawns `grok agent stdio` | **Yes** |
| Claude | `@anthropic-ai/claude-agent-sdk` | No |
| Codex | `effect-codex-app-server` (Codex app-server protocol) | No |
| OpenCode | `@opencode-ai/sdk/v2` (OpenCode server SDK) | No |

Key details:

- **`packages/effect-acp`** is their in-house Effect-TS implementation of the ACP client side, **code-generated from the official ACP schema releases** (`schema.unstable.json` from `agentclientprotocol/agent-client-protocol` GitHub releases), currently pinned to **ACP schema v0.11.3**, covering session/new, prompt, cancel, fork, list, resume, set_model/set_mode/config_option, etc.
- `apps/server/src/provider/acp/AcpSessionRuntime.ts` is the shared runtime that spawns an agent over stdio, runs JSON-RPC, and maps ACP session updates/tool calls into T3's canonical runtime events. Both ACP drivers share it.
- Vendor extensions ride on ACP's `_meta`/extension methods: e.g. Cursor's `cursor/list_available_models` for model discovery; xAI has `XAiAcpExtension.ts`.
- Notably, **T3 Code is not listed on agentclientprotocol.com's clients page** (checked 2026-08-26) despite being one of the larger consumers of the protocol.

## 4. Status & maturity

- Self-described as "**very very early**"; mostly closed to contributions (small fixes only).
- Free, MIT, no token resale. Install via `npx t3@latest`, brew/winget/AUR, or GitHub Releases; mobile apps on both stores. Very active (2,819 commits; latest commit same day as research).

## 5. Notes for fxcode (GPUI ACP manager)

- Same product category, opposite stack bet (Electron+TS vs GPUI+Rust). Their perf reputation suggests stack matters less than the architecture.
- Worth stealing: thin-client/server split with remote access as first-class; event-sourced orchestration with projections; per-turn checkpointing via hidden git refs; a canonical internal event model so each agent adapter normalizes into one shape; generated type-safe protocol bindings from the official ACP schema rather than hand-written JSON-RPC glue.

## Sources

- https://github.com/pingdotgg/t3code (README, file tree)
- https://t3.codes (official marketing site)
- https://raw.githubusercontent.com/pingdotgg/t3code/main/docs/internals/overview.md
- https://raw.githubusercontent.com/pingdotgg/t3code/main/docs/internals/providers.md
- https://raw.githubusercontent.com/pingdotgg/t3code/main/docs/internals/glossary.md
- Source code (local clone of pingdotgg/t3code @ a3a8cbd): `packages/effect-acp/{package.json,scripts/generate.ts,src/_generated/meta.gen.ts}`; `apps/server/src/provider/acp/{AcpSessionRuntime.ts,CursorAcpSupport.ts,GrokAcpSupport.ts,CursorDriver.ts,GrokDriver.ts}`; `apps/server/src/provider/Layers/{ClaudeAdapter.ts,CodexSessionRuntime.ts,OpenCodeAdapter.ts}`
- https://agentclientprotocol.com/get-started/clients (clients list; T3 Code absent)
- https://github.com/search?q=t3code&type=repositories (located the repo; community forks/themes)
