<script setup lang="ts">
import { ref, onMounted, onUnmounted, computed } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { t, initLocale } from "../i18n";

interface BoardContent {
  content_type: string;
  content: string;
}

const contentType = ref("text");
const content = ref("");
const hasContent = computed(() => content.value.length > 0);

const allowedTags = new Set([
  "a",
  "b",
  "blockquote",
  "br",
  "code",
  "em",
  "h1",
  "h2",
  "h3",
  "hr",
  "i",
  "li",
  "ol",
  "p",
  "pre",
  "span",
  "strong",
  "u",
  "ul",
]);

function sanitizeHtml(input: string): string {
  const parser = new DOMParser();
  const doc = parser.parseFromString(input, "text/html");
  const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_ELEMENT);
  const toUnwrap: Element[] = [];

  while (walker.nextNode()) {
    const node = walker.currentNode as Element;
    const tag = node.tagName.toLowerCase();

    if (!allowedTags.has(tag)) {
      toUnwrap.push(node);
      continue;
    }

    const attrs = Array.from(node.attributes);
    for (const attr of attrs) {
      const name = attr.name.toLowerCase();
      const value = attr.value;
      const isSafeLink =
        tag === "a" &&
        name === "href" &&
        (value.startsWith("https://") || value.startsWith("http://"));
      const keepAttr =
        (tag === "a" && (name === "target" || name === "rel")) || isSafeLink;
      if (!keepAttr) {
        node.removeAttribute(attr.name);
      }
    }

    if (tag === "a") {
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noopener noreferrer");
    }
  }

  for (const node of toUnwrap) {
    const parent = node.parentNode;
    if (!parent) continue;
    while (node.firstChild) {
      parent.insertBefore(node.firstChild, node);
    }
    parent.removeChild(node);
  }

  return doc.body.innerHTML;
}

const sanitizedHtml = computed(() => sanitizeHtml(content.value));

let unlisten: UnlistenFn | null = null;

function applyContent(payload: BoardContent) {
  contentType.value = payload.content_type;
  content.value = payload.content;
}

async function closeWindow() {
  await getCurrentWindow().close();
}

onMounted(async () => {
  await initLocale();
  // Fetch initial content stored by Rust (in case event was emitted before mount)
  try {
    const initial = await invoke<BoardContent | null>("get_board_content");
    if (initial) applyContent(initial);
  } catch {
    // Rust already logged the error
  }

  // Listen for subsequent updates
  unlisten = await listen<BoardContent>("agent-render-ui", (event) => {
    applyContent(event.payload);
  });
});

onUnmounted(() => {
  unlisten?.();
});
</script>

<template>
  <div class="board-container">
    <header class="board-header">
      <span class="board-title">{{ t('board.title') }}</span>
      <button class="board-close" @click="closeWindow" :title="t('board.close')">✕</button>
    </header>
    <main class="board-body" v-if="hasContent">
      <pre v-if="contentType === 'code'" class="board-code">{{ content }}</pre>
      <div v-else-if="contentType === 'html'" class="board-html" v-html="sanitizedHtml"></div>
      <div v-else class="board-text">{{ content }}</div>
    </main>
    <main class="board-body board-empty" v-else>
      <p>{{ t('board.empty') }}</p>
    </main>
  </div>
</template>

<style>
* {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}

body {
  font-family: -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
  background: #1a1a2e;
  color: #e0e0e0;
}

.board-container {
  display: flex;
  flex-direction: column;
  height: 100vh;
}

.board-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 16px;
  background: #16213e;
  border-bottom: 1px solid #0f3460;
  -webkit-user-select: none;
  user-select: none;
}

.board-title {
  font-size: 14px;
  font-weight: 600;
  color: #a0c4ff;
  letter-spacing: 0.5px;
}

.board-close {
  background: none;
  border: 1px solid rgba(255, 255, 255, 0.15);
  color: #ccc;
  font-size: 14px;
  width: 28px;
  height: 28px;
  border-radius: 6px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.15s, color 0.15s;
}

.board-close:hover {
  background: rgba(255, 60, 60, 0.3);
  color: #ff8080;
  border-color: rgba(255, 60, 60, 0.4);
}

.board-body {
  flex: 1;
  overflow: auto;
  padding: 20px;
}

.board-empty {
  display: flex;
  align-items: center;
  justify-content: center;
  color: #666;
  font-style: italic;
}

.board-text {
  white-space: pre-wrap;
  word-break: break-word;
  line-height: 1.7;
  font-size: 15px;
}

.board-code {
  background: #0d1117;
  border: 1px solid #30363d;
  border-radius: 8px;
  padding: 16px;
  font-family: "Cascadia Code", "Fira Code", Consolas, monospace;
  font-size: 13px;
  line-height: 1.6;
  overflow-x: auto;
  white-space: pre;
  color: #c9d1d9;
}

.board-html {
  line-height: 1.7;
  font-size: 15px;
}

.board-html h1, .board-html h2, .board-html h3 {
  color: #a0c4ff;
  margin: 16px 0 8px;
}

.board-html p {
  margin: 8px 0;
}

.board-html a {
  color: #58a6ff;
}

.board-html code {
  background: #0d1117;
  padding: 2px 6px;
  border-radius: 4px;
  font-size: 13px;
}
</style>
