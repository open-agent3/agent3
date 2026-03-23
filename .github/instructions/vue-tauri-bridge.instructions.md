---
applyTo: "src/**/*.{vue,ts}"
---

# Vue + Tauri Bridge Patterns — Agent3

## Core Principle

Frontend is a **thin visual layer** — no business logic. Only calls Tauri commands (`invoke`) and listens to events (`listen`/`emit`). All computation, state management, and I/O happen in Rust.

## Script Setup

Always use `<script setup lang="ts">` Composition API. No Options API.

```vue
<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, emit, type UnlistenFn } from "@tauri-apps/api/event";
</script>
```

## Tauri Bridge: invoke / listen / emit

**`invoke<T>(command, args?)`** — call Rust commands with type-safe returns:

```typescript
const providers = await invoke<Provider[]>("get_providers");
const ready = await invoke<boolean>("check_config_ready");
await invoke("save_provider", { provider });    // void command
invoke("agent_stop").catch(() => {});           // fire-and-forget
```

**`listen<T>(event, callback)`** — returns an `UnlistenFn` for cleanup:

```typescript
const unlisten = await listen<number>("agent-audio-energy", (event) => {
  energy.value = event.payload;
});
```

**`emit(event, payload?)`** — signal Rust from frontend:

```typescript
await emit("config-changed");  // triggers agent restart in Rust
```

## Event Cleanup Pattern

Store all `UnlistenFn` handles, call them on unmount:

```typescript
let unlisteners: UnlistenFn[] = [];

onMounted(async () => {
  const u1 = await listen<number>("agent-audio-energy", (e) => { /* ... */ });
  const u2 = await listen<string>("agent-wake-state", (e) => { /* ... */ });
  unlisteners.push(u1, u2);
});

onUnmounted(() => {
  for (const fn of unlisteners) fn();
  unlisteners = [];
});
```

For single listeners, the simpler pattern is fine:

```typescript
let unlisten: UnlistenFn | null = null;

onMounted(async () => {
  unlisten = await listen<BoardContent>("agent-render-ui", (e) => applyContent(e.payload));
});

onUnmounted(() => unlisten?.());
```

## Registered Events

| Event | Direction | Payload |
|-------|-----------|---------|
| `agent-status` | Rust → FE | `{ state: string, message?: string }` |
| `agent-audio-energy` | Rust → FE | `number` (0–1) |
| `agent-playback-energy` | Rust → FE | `number` (0–1) |
| `agent-wake-state` | Rust → FE | `"sleeping" \| "awakened" \| "listening"` |
| `agent-render-ui` | Rust → FE | `{ content_type: string, content: string }` |
| `config-ready` | Rust → FE | (none) |
| `config-changed` | FE → Rust | (none) |

Event names use **kebab-case**.

## CSS Variables from Rust

Energy values from Rust drive CSS animations via `--audio-energy`:

```typescript
// In requestAnimationFrame loop
document.documentElement.style.setProperty(
  '--audio-energy',
  Math.min(1, Math.max(0, audioEnergy.value)).toString()
);
```

```css
.edge-glow-container {
  transform: scaleY(calc(0.4 + var(--audio-energy) * 1.2));
  opacity: calc(0.5 + var(--audio-energy) * 0.5);
}
```

## Styling

Pure CSS + CSS variables only — **no Tailwind**. Use `<style scoped>` for component styles.

## TypeScript

- Strict mode with `noUnusedLocals` + `noUnusedParameters`
- Define interfaces for all Rust-facing types:

```typescript
type ProviderType = "openai" | "gemini" | "deepseek";

interface Provider {
  id: string;
  name: string;
  base_url: string;
  api_key: string;
  model: string;
  is_active: boolean;
  provider_type: ProviderType;
  role: string;
}
```

## Config Change Flow

After any settings mutation, always emit + refresh:

```typescript
await invoke("save_provider", { provider });
await emit("config-changed");                          // Rust auto-restarts agent
providers.value = await invoke<Provider[]>("get_providers");  // refresh UI
```
