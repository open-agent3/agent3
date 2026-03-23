import { createApp } from "vue";
import { invoke } from "@tauri-apps/api/core";
import App from "./App.vue";

// Hook console methods to forward logs to Rust terminal
const _log = console.log;
const _warn = console.warn;
const _error = console.error;

function forward(level: string, args: unknown[]) {
  const msg = args.map(a => typeof a === "string" ? a : JSON.stringify(a)).join(" ");
  invoke("log_from_frontend", { level, message: msg }).catch(() => {});
}

console.log = (...args: unknown[]) => { _log.apply(console, args); forward("log", args); };
console.warn = (...args: unknown[]) => { _warn.apply(console, args); forward("warn", args); };
console.error = (...args: unknown[]) => { _error.apply(console, args); forward("error", args); };

createApp(App).mount("#app");
