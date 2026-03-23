---
applyTo: "src-tauri/**/*.rs"
---

# Rust Patterns — Agent3

## Error Handling

Tauri commands return `Result<T, String>`. Convert all errors with `.map_err(|e| e.to_string())?`:

```rust
#[tauri::command]
pub fn example(state: tauri::State<'_, DbState>) -> Result<Vec<Item>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare("SELECT ...").map_err(|e| e.to_string())?;
    // ...
    Ok(result)
}
```

Never use `.unwrap()` in Tauri commands — always propagate with `?`.

## Async: Channels + Spawn + Select

Create channels with fixed capacity, spawn tasks, multiplex with `tokio::select!`:

```rust
let (tx, mut rx) = mpsc::channel::<Event>(64);
let task = tokio::spawn(async move {
    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(e) => handle(e).await,
                    None => break,
                }
            }
            _ = cancel.recv() => break,
        }
    }
});
```

Channel capacity is consistently **64** across the codebase. The `select!` pattern is the standard way to multiplex multiple event sources.

## State Management

**Newtype wrappers** for Tauri managed state:

```rust
pub struct DbState(pub Mutex<Connection>);          // Always-present state
pub struct BoardState(pub Mutex<Option<Content>>);   // Optional state
```

**`Mutex<Option<T>>`** for components that start/stop (session, audio):

```rust
pub struct AgentState {
    pub session: Mutex<Option<SessionHandle>>,
    pub audio: Mutex<Option<AudioHandle>>,
}
```

**`Arc<AtomicU8/AtomicBool>`** for lock-free cross-thread state (audio wake state, recording flag):

```rust
let state = Arc::new(AtomicU8::new(0));
state.store(value, Ordering::Relaxed);
let current = state.load(Ordering::Relaxed);
```

## Logging

Always prefix with module name in brackets:

```rust
log::info!("[Session] Starting with provider: {} (type={})", name, provider_type);
log::info!("[Memory] Persisted {} message", role);
log::info!("[Audio] Device: {}", device.name().unwrap_or_default());
log::info!("[Tools] Dispatching: {}({})", name, args_json);
log::error!("[Tools] {} \u2192 Error: {}", name, e);
```

Registered prefixes: `[Session]`, `[Memory]`, `[Audio]`, `[Playback]`, `[Tools]`, `[Ambient]`, `[Scheduler]`, `[RealtimeWS]`, `[Wakeword]`.

## Import Order

`std` → external crates → `crate::` internal. No glob imports.

```rust
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

use crate::agent::{task_manager, tools};
use crate::db::DbState;
```

## Channel Types

Sensory→Orchestrator (`SensoryEvent`): `UserTranscript`, `AssistantTranscript`, `EscalateRequest`, `UserInterrupt`, `Connected`, `Reconnected`, `Disconnected`.

Orchestrator→Sensory (`OrchestratorCommand`): `InjectSpeech`, `UpdateInstructions`, `CompleteFunctionCall`.

## Tool Dispatch

Tools are defined as JSON schemas in `tools.rs` and dispatched via match:

```rust
let result: Result<String, String> = match name {
    "exec_shell" => {
        let command = args["command"].as_str().unwrap_or("").to_string();
        system_api::exec_shell(command)
    }
    "type_text" => {
        let text = args["text"].as_str().unwrap_or("").to_string();
        system_api::type_text(text).map(|_| "OK".to_string())
    }
    _ => Err(format!("Unknown tool: {}", name)),
};
```

Extract args with `args["field"].as_str().unwrap_or("")`. Return `"OK"` for void operations.

## Unsafe Send/Sync

cpal `Stream` needs explicit trait impls for cross-thread usage. Always add a comment explaining why:

```rust
// cpal::Stream is Send on Windows WASAPI;
// macOS CoreAudio may require std::thread, using unsafe as fallback
unsafe impl Send for AudioHandle {}
unsafe impl Sync for AudioHandle {}
```
