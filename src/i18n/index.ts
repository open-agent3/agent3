/**
 * i18n — Lightweight internationalization module
 *
 * Zero-dependency, reactive locale with flat key lookup.
 * Supports English and Chinese. Detects system language on init,
 * user can override in Settings.
 */

import { ref, type Ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import en from "./en.json";
import zh from "./zh.json";

export type Locale = "en" | "zh";

type Dict = Record<string, unknown>;

const dictionaries: Record<Locale, Dict> = { en, zh };

/** Current active locale (reactive) */
export const locale: Ref<Locale> = ref("en");

let currentDict: Dict = en;

/**
 * Lookup a nested key like "config.tab_general" from the current dictionary.
 * Returns the key itself if not found (makes missing translations visible).
 */
export function t(key: string): string {
  // trigger reactivity properly without warnings
  if (locale.value) {
    /* */
  }
  const parts = key.split(".");
  let node: unknown = currentDict;
  for (const part of parts) {
    if (node && typeof node === "object" && part in (node as Dict)) {
      node = (node as Dict)[part];
    } else {
      return key; // fallback: show key as-is
    }
  }
  return typeof node === "string" ? node : key;
}

/**
 * Switch locale and persist to backend settings.
 */
export async function setLocale(lang: Locale): Promise<void> {
  currentDict = dictionaries[lang] ?? en;
  locale.value = lang;
  try {
    await invoke("set_setting", { key: "language", value: lang });
  } catch {
    // best-effort persist
  }
}

/**
 * Detect initial locale: read user preference from backend,
 * fallback to browser language, default to English.
 */
export async function initLocale(): Promise<void> {
  let lang: Locale = "en";
  try {
    const saved = await invoke<string>("get_setting", { key: "language" });
    if (saved === "zh" || saved === "en") {
      lang = saved as Locale;
    } else if (!saved) {
      // No user preference — detect from browser
      lang = navigator.language.startsWith("zh") ? "zh" : "en";
    }
  } catch {
    lang = navigator.language.startsWith("zh") ? "zh" : "en";
  }
  currentDict = dictionaries[lang] ?? en;
  locale.value = lang;
}
