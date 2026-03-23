<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { AgentBridge } from "./core/AgentBridge";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { initLocale } from "./i18n";

let pipeline: AgentBridge | null = null;
const audioEnergy = ref(0);
const wakeState = ref<'sleeping' | 'awakened' | 'listening'>('sleeping');
const hasError = ref(false);

// ── Subagent Ghost UI ──
interface SubagentLog {
  task_id: string;
  status: string;
  message: string;
  ts: number;
}
const subagentLogs = ref<SubagentLog[]>([]);
let ghostFadeTimer: ReturnType<typeof setTimeout> | null = null;
let subagentUnlisten: UnlistenFn | null = null;

// ── Status Toast ──
const toastText = ref("");
const toastLevel = ref<"info" | "error" | "warn">("info");
const toastVisible = ref(false);
const toastActionLabel = ref("");
const toastActionHandler = ref<(() => void) | null>(null);
let toastTimer: ReturnType<typeof setTimeout> | null = null;

function showToast(
  text: string,
  level: "info" | "error" | "warn" = "info",
  durationMs = 3000,
  action?: { label: string; onClick: () => void }
) {
  toastText.value = text;
  toastLevel.value = level;
  toastActionLabel.value = action?.label || "";
  toastActionHandler.value = action?.onClick || null;
  toastVisible.value = true;
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = null;
  if (durationMs > 0) {
    toastTimer = setTimeout(() => {
      toastVisible.value = false;
    }, durationMs);
  }
  // durationMs === 0 → persistent display, no timer set
}

function handleToastAction() {
  const handler = toastActionHandler.value;
  if (!handler) return;
  handler();
  toastVisible.value = false;
}

// Render loop variable
let renderFrameId: number;

onMounted(async () => {
  await initLocale();
  pipeline = new AgentBridge(showToast);
  
  // Connection state → error flag + clear persistent toast
  pipeline.bindConnectionState((state) => {
    hasError.value = state === 'error';
    if (state === 'connected') {
      toastVisible.value = false;
    }
  });

  // Wake state → glow driver
  pipeline.bindWakeState((state) => {
    wakeState.value = state;
    hasError.value = false;
  });

  // Energy output (from Rust native capture + Rust playback)
  pipeline.bindEnergyOutput((energy) => {
    const amplified = Math.min(1, energy * 4);
    audioEnergy.value += (amplified - audioEnergy.value) * 0.2;
  });

  pipeline.start();

  // Subagent Ghost UI listener
  subagentUnlisten = await listen<SubagentLog>("subagent-log", (event) => {
    const log = { ...event.payload, ts: Date.now() };
    subagentLogs.value = [...subagentLogs.value.slice(-9), log];
    // Auto-clear after 8s of inactivity
    if (ghostFadeTimer) clearTimeout(ghostFadeTimer);
    ghostFadeTimer = setTimeout(() => {
      subagentLogs.value = [];
    }, 8000);
  });

  // Render loop driving CSS variables
  const updateCSS = () => {
    document.documentElement.style.setProperty('--audio-energy', Math.min(1, Math.max(0, audioEnergy.value)).toString());
    renderFrameId = requestAnimationFrame(updateCSS);
  };
  updateCSS();
});

onUnmounted(() => {
  pipeline?.destroy();
  subagentUnlisten?.();
  if (ghostFadeTimer) clearTimeout(ghostFadeTimer);
  cancelAnimationFrame(renderFrameId);
});
</script>

<template>
  <div class="edge-glow-container" :class="[wakeState, { error: hasError }]">
    <div class="glow-layer color-brand-1"></div>
    <div class="glow-layer color-brand-2"></div>
    <div class="glow-layer color-brand-3"></div>
  </div>

  <!-- Layer 1: Status Toast -->
  <Transition name="toast">
    <div v-if="toastVisible" :class="['status-toast', toastLevel, { actionable: !!toastActionHandler }]">
      <span>{{ toastText }}</span>
      <button v-if="toastActionLabel" class="toast-action" @click.stop="handleToastAction">
        {{ toastActionLabel }}
      </button>
    </div>
  </Transition>

  <!-- Layer 2: Subagent Ghost UI -->
  <Transition name="ghost">
    <div v-if="subagentLogs.length" class="ghost-terminal">
      <div v-for="log in subagentLogs" :key="log.task_id + log.ts" class="ghost-line">
        <span class="ghost-id">#{{ log.task_id }}</span>
        <span :class="['ghost-status', log.status]">{{ log.status }}</span>
        <span class="ghost-msg">{{ log.message.slice(0, 80) }}</span>
      </div>
    </div>
  </Transition>
</template>

<style>
html, body {
  margin: 0;
  padding: 0;
  overflow: hidden;
  background-color: transparent !important;
  width: 100vw;
  height: 100vh;
}

:root {
  --audio-energy: 0;
}

.edge-glow-container {
  position: fixed;
  bottom: -10px;
  left: 0;
  width: 100vw;
  height: 260px;
  pointer-events: none;
  z-index: 0;
  display: flex;
  justify-content: center;
  align-items: flex-end;
  filter: blur(50px);
  transform-origin: bottom center;
  /* Driven purely by AI playback energy — invisible when silent */
  transform: scaleY(calc(0.3 + var(--audio-energy) * 1.5));
  opacity: var(--audio-energy);
  transition: opacity 0.3s ease, transform 0.15s ease;
}

/* Listening (idle, no AI speech): stay invisible, energy drives appearance */
.edge-glow-container.listening {
  /* No animation — glow only appears when --audio-energy > 0 */
}

/* Awakening (WS connecting) */
.edge-glow-container.awakened {
  opacity: 0.3;
  animation: pulse-connecting 1.5s ease-in-out infinite;
}

@keyframes pulse-connecting {
  0%, 100% { opacity: 0.3; transform: scaleY(0.5); }
  50%      { opacity: 0.55; transform: scaleY(0.75); }
}

/* Sleeping — very faint breathing */
.edge-glow-container.sleeping {
  opacity: 0.08;
  animation: breathe-dim 6s ease-in-out infinite;
}

@keyframes breathe-dim {
  0%, 100% { opacity: 0.08; transform: scaleY(0.3); }
  50%      { opacity: 0.18; transform: scaleY(0.45); }
}

.edge-glow-container.error .glow-layer {
  background: red !important;
}

.glow-layer {
  width: 40vw;
  height: 120px;
  border-radius: 50%;
}

.color-brand-1 {
  background: rgba(255, 81, 47, 0.9); /* #FF512F */
  transform: translateX(8vw) scaleY(calc(1 + var(--audio-energy) * 2));
}
.color-brand-2 {
  background: rgba(221, 36, 118, 0.9); /* #DD2476 */
  transform: translateX(0) scaleY(calc(1 + var(--audio-energy) * 2.5));
}
.color-brand-3 {
  background: rgba(255, 105, 53, 0.8); /* Blend of orange/pink */
  transform: translateX(-8vw) scaleY(calc(1 + var(--audio-energy) * 2));
}

#sandbox-layer {
  position: fixed;
  top: 0;
  left: 0;
  width: 100vw;
  height: 100vh;
  pointer-events: none;
  z-index: 1;
}

.status-toast {
  position: fixed;
  bottom: 120px;
  left: 50%;
  transform: translateX(-50%);
  padding: 10px 28px;
  border-radius: 20px;
  font-family: -apple-system, 'Segoe UI', sans-serif;
  font-size: 14px;
  letter-spacing: 0.3px;
  pointer-events: none;
  z-index: 100;
  color: white;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(10px);
  transition: all 0.3s ease;
}

.status-toast.actionable {
  pointer-events: auto;
}

.toast-action {
  margin-left: 10px;
  border: 1px solid rgba(255, 255, 255, 0.35);
  background: rgba(255, 255, 255, 0.12);
  color: #fff;
  border-radius: 999px;
  padding: 4px 10px;
  font-size: 12px;
  cursor: pointer;
}

.toast-action:hover {
  background: rgba(255, 255, 255, 0.2);
}

.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.3s ease, transform 0.3s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translate(-50%, 20px);
}



.status-toast.info {
  background: rgba(0, 80, 200, 0.7);
  color: #fff;
  border: 1px solid rgba(80, 170, 255, 0.5);
}

.status-toast.warn {
  background: rgba(180, 110, 0, 0.75);
  color: #fff;
  border: 1px solid rgba(255, 180, 50, 0.5);
  animation: toast-pulse 2s ease-in-out infinite;
}

@keyframes toast-pulse {
  0%, 100% { opacity: 0.85; }
  50%      { opacity: 1; }
}

.status-toast.error {
  background: rgba(180, 20, 20, 0.75);
  color: #fff;
  border: 1px solid rgba(255, 80, 80, 0.5);
}

/* ── Ghost Terminal (Subagent Logs) ── */
.ghost-terminal {
  position: fixed;
  bottom: 16px;
  left: 16px;
  max-width: 420px;
  padding: 8px 12px;
  border-radius: 6px;
  background: rgba(0, 0, 0, 0.5);
  backdrop-filter: blur(6px);
  border: 1px solid rgba(0, 255, 65, 0.15);
  font-family: 'Cascadia Code', 'Fira Code', 'Consolas', monospace;
  font-size: 11px;
  line-height: 1.5;
  pointer-events: none;
  z-index: 50;
}

.ghost-enter-active,
.ghost-leave-active {
  transition: opacity 0.4s ease;
}
.ghost-enter-from,
.ghost-leave-to {
  opacity: 0;
}

.ghost-line {
  display: flex;
  gap: 6px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  color: rgba(0, 255, 65, 0.7);
}

.ghost-id {
  color: rgba(0, 255, 65, 0.4);
  flex-shrink: 0;
}

.ghost-status {
  flex-shrink: 0;
  font-weight: 600;
}
.ghost-status.started { color: rgba(100, 200, 255, 0.8); }
.ghost-status.thinking { color: rgba(0, 255, 65, 0.6); }
.ghost-status.tool { color: rgba(255, 200, 50, 0.8); }
.ghost-status.result { color: rgba(0, 255, 65, 0.5); }
.ghost-status.waiting { color: rgba(255, 150, 50, 0.9); }
.ghost-status.resumed { color: rgba(100, 200, 255, 0.8); }
.ghost-status.completed { color: rgba(0, 255, 65, 0.9); }
.ghost-status.error { color: rgba(255, 80, 80, 0.9); }

.ghost-msg {
  overflow: hidden;
  text-overflow: ellipsis;
  color: rgba(0, 255, 65, 0.5);
}
</style>
