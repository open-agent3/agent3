# Agent3 — Copilot Instructions

> System-level ambient AI voice agent, inspired by the movie *Her*.
> Tauri 2.0 + Vue 3 + TypeScript + Rust + SQLite

For deep architecture details and data-flow diagrams, see [docs/ARCHITECTURE.md](../docs/ARCHITECTURE.md).

## Architecture

**First principle**: Think from first principles when making architectural decisions and solving problems. Break down complex issues into their fundamental truths before choosing a solution.
**Native preference**: For every feature, ask "Can Rust do this natively?" — if yes, don't route through frontend or external processes. Audio, networking, files, and input control are all native Rust. The WebView is a transparent visual layer only — no business logic.

Session + MemoryStore architecture, all heavy logic in Rust:

```
Session (session.rs)  ←holds→  MemoryStore (memory.rs)
  Realtime WS voice I/O          Synchronous persistence
  + direct tool execution         System instructions builder
  + inject_rx channel             Context retrieval for reconnection
```

- **Session**: cpal capture → WS → cpal playback, manages wake state machine, executes tools directly via task_manager, persists transcripts via MemoryStore
- **MemoryStore**: Synchronous struct held by Session — persists transcripts/tool results to SQLite, builds system instructions, retrieves recent context for reconnection

External event injection uses a simple `mpsc::channel::<String>` (`inject_tx` / `inject_rx`)

### Multi-Page Windows

| Window | Entry | Purpose |
|--------|-------|---------|
| main (index.html) | src/App.vue | Fullscreen transparent click-through base — Edge Glow effect |
| config (config.html) | src/config/ConfigApp.vue | Settings panel, opened from system tray |
| board (board.html) | src/board/BoardApp.vue | Agent output display, created on demand |

## Build & Dev

```bash
pnpm dev              # Vite dev server (port 1420)
pnpm build            # vue-tsc + vite build (multi-page → dist/)
cargo check           # Rust type-check (run inside src-tauri/)
pnpm tauri dev        # Full dev: frontend + Rust hot-reload
pnpm tauri build      # Production build
```

### Dev pitfalls

- Main window is **transparent + always-on-top + click-through** — you won't see it in the taskbar. Open Settings from the **system tray** icon.
- `cargo check` must run inside `src-tauri/`, not the workspace root.
- Vite dev server locks to port **1420**; Tauri expects this exact port.
- Rust rebuilds are triggered by Cargo, not Vite — the Vite watcher ignores `src-tauri/**`.

## Code Conventions

### General

- File/module names: English `snake_case`
- Comments and documentation: **English**
- Event names: `kebab-case` (`agent-transcript`, `config-ready`, `config-changed`)
- Frontend is a thin layer: only calls Tauri commands + listens to events — no business logic

### Rust

- Tauri commands return `Result<T, String>` — convert errors with `.map_err(|e| e.to_string())?`
- Logging: `log` crate with module prefix: `[Session]`, `[Memory]`, `[Audio]`, etc.
- Async: `tokio::spawn` + `tokio::select!` multiplexing, `mpsc::channel` for communication
- State: newtype wrapper pattern — `pub struct DbState(pub Mutex<Connection>)`
- Import order: `std` → external crates → `crate::` internal — no glob imports

### Frontend (Vue 3 + TypeScript)

- `<script setup lang="ts">` Composition API only — no Options API
- Pure CSS + CSS variables (`--audio-energy`) — no Tailwind
- Tauri bridge: `invoke()` for commands, `listen()` for events, `emit()` for outbound events
- TypeScript strict mode with `noUnusedLocals` + `noUnusedParameters`
- Cleanup pattern: store `unlisten` handles in `onMounted`, call them in `onUnmounted`

## Database

- SQLite WAL mode, path: `<app_data_dir>/agent3.db`
- Migrations: `PRAGMA user_version` incremented per version, functions appended to `MIGRATIONS` array in `db.rs`
- Core tables: `llm_providers` (provider config), `app_settings` (KV store), `core_profile` (L1 Cache / high priority settings), `episodic_logs` (chronological context window), `kg_nodes` & `kg_edges` & `kg_nodes_fts` (Knowledge Graph relational memory).

## Key Patterns

- **All-native audio**: cpal capture → resample to 24kHz → base64 → WS → cpal playback — never passes through frontend
- **Wakeword**: rustpotter DTW detection, state machine `Sleeping → Awakened → Listening`
- **Tool system**: `tools.rs` defines multiple OS tools (shell, keyboard, mouse, vision, board, cognitive memory, etc.), all registered on Realtime WS session. `task_manager.rs` dispatches concurrently with 3s filler speech on timeout
- **Multi-provider**: `RealtimeProtocol` trait abstracts OpenAI / Gemini WebSocket protocol differences
- **User interrupt**: `speech_started` WS event (server VAD) triggers `task_mgr.abort_all()` — InputTranscript does NOT trigger interrupts
- **Config flow**: Frontend `emit("config-changed")` → Rust listener in `lib.rs` → auto-restart agent → `emit("config-ready")`
- **Reconnection context**: After WS reconnect, `memory.recent_context(20)` injects last 20 messages via `conversation.item.create` to maintain continuity
