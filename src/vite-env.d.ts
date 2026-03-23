/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}

declare module "@tauri-apps/plugin-autostart" {
  export function enable(): Promise<void>;
  export function disable(): Promise<void>;
  export function isEnabled(): Promise<boolean>;
}
