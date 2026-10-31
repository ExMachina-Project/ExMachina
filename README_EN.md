
# ExMachina — Rationalist Agents

> [!WARNING]
> This project is in pre-alpha. Interfaces and data formats may break without notice. Do not use in production.
> Contracts are defined in [docs/](./docs) (Chinese, source of truth).

<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="./docs/logo-dark.svg">
  <img src="./docs/logo.svg" alt="EXMACHINA" width="132" height="132">
</picture>
<br>
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="./docs/wordmark-dark.svg">
  <img src="./docs/wordmark.svg" alt="EXMACHINA" width="600">
</picture>

![Stage](https://img.shields.io/badge/stage-pre--alpha-orange)
![Rust](https://img.shields.io/badge/Rust-stable-dea584?logo=rust&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-blue)
![Tests](https://img.shields.io/badge/cargo%20test-passing-3fb950)
![WebUI](https://img.shields.io/badge/WebUI-React%2019%20%C2%B7%20Vite%20%C2%B7%20antd%206-61dafb?logo=react&logoColor=white)

</div>

<p align="center">
  <a href="./README.md">简体中文</a> · <a href="./README_EN.md">English</a>
</p>

---

## Quick Start

Prerequisites: [Rust](https://rustup.rs) (stable) and Node.js ≥ 22 (WebUI build only).

**Install** (prebuilt release or from source, picked interactively):

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/ExMachina-Project/ExMachina/main/scripts/install.sh | bash
```

```batch
:: Windows (download, then run)
curl -fsSL -o install.bat https://raw.githubusercontent.com/ExMachina-Project/ExMachina/main/scripts/install.bat
install.bat
```

The script asks which method to use: a prebuilt release (recommended, no Rust / Node.js needed) or building from source; pass `--bin` / `--src` to skip the prompt.

Opens http://127.0.0.1:4173 — configure your API key on the Providers page.

**Manual build**:

```bash
git clone https://github.com/ExMachina-Project/ExMachina.git && cd ExMachina
npm install && npm run build:webui
cargo run -p exm-gateway --release        # http://127.0.0.1:4173
```

**Terminal workbench** (`exm` with no subcommand): a fullscreen ratatui TUI (message stream / input / status bar) with Chat, Approvals and plan pages — native coding on the terminal. `exm chat` keeps the classic readline panel.

## One Agent, or a Team

EXMACHINA gives you two interaction targets, chosen by task weight — both share the same memory, model, and tool foundations.

**A single agent** — direct conversation, direct work. No orchestration overhead: system prompt + history + tool loop, and the answer. The full tool surface (terminal / files / patch / browser / computer use / MCP), layered memory, multi-provider models, persona and experience optimization — one agent is enough for daily tasks.

**An agent group** — for tasks too heavy for one. A single assistant's ceiling is clear: **one context, one chain of thought, one answer** — the larger the task, the more constraints and evidence get lost mid-chain, and you can't trace how a conclusion was reached or intervene when it goes off track. Group mode turns "accomplish something complex" into an **auditable organizational behavior**:

- **Division of labor** — The orchestrator does one thing: lock boundaries, decompose tasks, dispatch, arbitrate, converge. 39 sub-agents each hold a domain (research, architecture, implementation, verification, decision-making, operations, security, documentation…), producing in parallel.
- **Contract-driven** — Every sub-agent's output must pass SyncReport structural + business validation. Failures auto-rewrite (max 2 attempts), then escalate to the orchestrator.
- **Full auditability** — Three ledgers (task / evidence / risk) evolve per round, so every answer can answer "what's the evidence and where's the risk".
- **Composable** — Groups are the unit of isolation and switching. Build any team structure you need; the primary agent can expand its own roster mid-task.

## Core Features

**Single agent (direct service)**
- Answers without orchestration: system prompt + history + tool loop; the built-in `default` agent works out of the box
- Full tool surface with the same approval / audit gates as groups
- Shares the layered memory and multi-provider model system; persona (SOUL) and experience optimization apply too

**Agent group (cluster intelligence · core)**
- Task decomposition into DAG, topological scheduling with concurrency pool, dynamic arbitration nodes
- SyncReport contract enforcement with auto-rewrite; full event sourcing for replay
- Agent groups with workspace isolation; primary agent can expand roster mid-task

**Memory & evolution**
- Hybrid retrieval: lexical (CJK bigram + inverted index) + semantic (embedding cosine, provider API)
- Auto-write: session digests, boundary decisions, A/B evidence, failure lessons
- Experience optimization: lessons distilled into behavioral notes, injected into future dispatches
- Reliability stats feed back into orchestrator routing

**Platform & models**
- 4 protocols: OpenAI-compatible, Anthropic, Google Gemini, Azure OpenAI
- Per-agent / per-group default model resolution (agent → group → global)
- Multi-key pools with sticky load balancing (rotate only on quota errors)
- Model failover chains with cooldown (45s) — switch only before content is emitted
- Native function calling across all 4 protocols; text-protocol fallback for mock/compat

**Voice & multimodal**
- Voice input: WebUI mic → API transcription (Whisper) → text; Telegram voice messages supported
- Image input: attach up to 4 images per message (vision-capable models)

**Distributed execution**
- Old devices join as worker nodes (`exm worker --url ws://hub:4173/worker`)
- Workers execute sub-agent tasks with their own local models (e.g., Ollama)
- Automatic fallback to local execution on failure / timeout

**Security & reliability**
- Terminal command approval gate (off / risky / always) + prefix allowlist + audit logging
- Session chat allowlist per channel
- Context compaction: rolling summary + recent window for multi-turn memory
- Session token budget (estimated chars/4, 0 = unlimited)
- Interrupted-session resume on gateway boot

**MCP client**
- stdio + Streamable HTTP transports, lazy initialize, per-agent tool visibility
- Tools mounted as `mcp:server:tool` into native function calling

**Chat as coding**
- No separate coding workbench: reference workspace files with `@` right in the chat composer
- `patch` unified-diff tool (atomic apply — any file failing validation rejects the whole patch), per-line diff coloring on tool cards with one-click rollback (checkpoint-linked), terminal output panel
- Destructive git operations go through the in-chat approval gate

**Computer Use**
- `computer` tool: screenshots fed back to vision models + mouse/keyboard control of the local desktop
- Off by default; input actions pass the approval gate, action throttling prevents runaway clicks

**Plugin system**
- `plugins/` directory as the universal capability package: declarative plugins (inline HTTP tools in a manifest — zero code, credential variable injection, two-level response trimming), process plugins (any language via stdio JSON-RPC, handshake version negotiation, crash back-off restart), built-in plugins (Rust trait; video/rss as references)
- Ecosystem: one-command install/update from tar.gz / git / index sources (CLI `exm plugin` and the WebUI plugin catalog share the pipeline); end-to-end masked credential management
- WebUI plugin page: enable/disable, uninstall, catalog install, health at a glance; tool namespace `<plugin>__<tool>`, audit entries carry plugin attribution

## WebUI Console

14 panels, 2 theme presets × 5 accents: Chat (target switching, streaming, images, TTS), Agents (roster, persona, adaptation), Groups (roster + default model), Nexus (experimental links), Task Graph (session-labeled DAG), Three Ledgers, Models (providers, key pools, capability models), Memory (deep db or memory.md file mode), Skills, Automations (cron / one-off / queue), Approvals, Channels, Events (timeline + tool audit), Settings (backend-schema driven).

## Documentation

| Doc | Content |
|-----|---------|
| [docs/Installation](./docs/安装指南.md) | Setup, provider config, Docker |
| [docs/Usage](./docs/使用指南.md) | Full CLI + WebUI handbook |
| [docs/Architecture](./docs/架构与设计.md) | Design principles, core mechanisms |
| [docs/Protocol](./docs/协议与契约.md) | Entity schemas, REST/WS API, extension points |
| [PROGRESS](./PROGRESS.md) | Development log |

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) (bilingual zh/en).

## License

[MIT](./LICENSE)