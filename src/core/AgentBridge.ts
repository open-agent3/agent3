/**
 * AgentBridge — Thin frontend bridge layer
 *
 * All networking, tool calls, audio capture, and audio playback are handled in the Rust backend.
 * This module only handles:
 *   1. Invoking Tauri commands to start/stop the Rust pipeline
 *   2. Listening for status / transcript / energy / wake-state events emitted by Rust
 */

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { t } from "../i18n";

export type StatusCallback = (
  text: string,
  level: "info" | "error" | "warn",
  durationMs?: number,
  action?: { label: string; onClick: () => void }
) => void;

interface StatusPayload {
  state: string;
  message?: string;
}

export class AgentBridge {
  private showStatus: StatusCallback;
  private onEnergyOutput: ((energy: number) => void) | null = null;
  private onConnectionStateChange: ((state: 'connected' | 'disconnected' | 'error' | 'waiting-for-mic') => void) | null = null;
  private onWakeStateChange: ((state: 'sleeping' | 'awakened' | 'listening') => void) | null = null;

  private _running = false;
  private unlisteners: UnlistenFn[] = [];

  constructor(showStatus?: StatusCallback) {
    this.showStatus = showStatus ?? (() => {});
  }

  bindEnergyOutput(callback: (energy: number) => void): void {
    this.onEnergyOutput = callback;
  }

  bindConnectionState(callback: (state: 'connected' | 'disconnected' | 'error' | 'waiting-for-mic') => void): void {
    this.onConnectionStateChange = callback;
  }

  bindWakeState(callback: (state: 'sleeping' | 'awakened' | 'listening') => void): void {
    this.onWakeStateChange = callback;
  }

  async start(): Promise<void> {
    if (this._running) return;

    // 1. Register backend event listeners
    await this.setupListeners();

    // 2. Check config readiness
    let configReady = false;
    try {
      configReady = await invoke<boolean>("check_config_ready");
    } catch {
      configReady = false;
    }

    if (configReady) {
      await this.startAgent();
    } else {
      this.showStatus(t("bridge.config_required"), "warn", 6000);
      let unlisten: UnlistenFn | null = null;
      unlisten = await listen("config-ready", async () => {
        if (unlisten) unlisten();
        await this.startAgent();
      });
      this.unlisteners.push(() => { if (unlisten) unlisten(); });
    }

    this._running = true;
  }

  /** Start the Rust agent pipeline */
  private async startAgent(): Promise<void> {
    try {
      await invoke("agent_start");
    } catch (err: any) {
      this.showStatus(
        typeof err === "string" ? err : t("bridge.start_failed"),
        "warn",
        5000
      );
    }
  }

  stop(): void {
    // Clean up event listeners
    for (const fn of this.unlisteners) fn();
    this.unlisteners = [];

    // Notify Rust to stop pipeline (fire-and-forget)
    invoke("agent_stop").catch(() => {});

    this._running = false;
  }

  async restart(): Promise<void> {
    this.stop();
    await this.start();
  }

  get running(): boolean {
    return this._running;
  }

  destroy(): void {
    this.stop();
  }

  // ── Internal: Register backend event listeners ──

  private async setupListeners(): Promise<void> {
    // Clean up old listeners (safety)
    for (const fn of this.unlisteners) fn();
    this.unlisteners = [];

    // Pipeline status
    const u1 = await listen<StatusPayload>("agent-status", (event) => {
      const { state, message } = event.payload;
      switch (state) {
        case "connected":
          this.onConnectionStateChange?.("connected");
          this.showStatus(t("bridge.connected"), "info", 3000);
          break;
        case "disconnected":
          this.onConnectionStateChange?.("disconnected");
          this.showStatus(message || t("bridge.reconnecting"), "warn", 4000);
          break;
        case "error":
          this.onConnectionStateChange?.("error");
          this.showStatus(message || t("bridge.connection_failed"), "error", 5000);
          break;
        case "no-provider":
          // no-provider state mapping to disconnected in UI
          this.onConnectionStateChange?.("disconnected");
          this.showStatus(
            message || t("bridge.no_provider"),
            "warn",
            6000
          );
          break;
        case "waiting-for-mic":
          this.onConnectionStateChange?.("waiting-for-mic");
          this.showStatus(
            message || t("bridge.no_mic"),
            "warn",
            0
          );
          break;
        case "wakeword-nudge":
          this.showStatus(
            message || t("bridge.wakeword_nudge"),
            "warn",
            9000,
            {
              label: t("bridge.open_settings"),
              onClick: () => {
                invoke("open_settings_window", { focusWakeword: true }).catch(() => {});
              },
            }
          );
          break;
        case "stopped":
          break; // silent
      }
    });

    // Playback energy (Rust rodio) — only AI speech drives the Edge Glow
    const u5 = await listen<number>("agent-playback-energy", (event) => {
      this.onEnergyOutput?.(event.payload);
    });

    // Wake state
    const u6 = await listen<string>("agent-wake-state", (event) => {
      this.onWakeStateChange?.(event.payload as 'sleeping' | 'awakened' | 'listening');
    });

    this.unlisteners.push(u1, u5, u6);
  }
}
