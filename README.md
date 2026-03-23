# Agent3

[English](README.md) | [中文说明](README.zh-CN.md)

[![CI](https://github.com/open-agent3/agent3/actions/workflows/ci.yml/badge.svg)](https://github.com/open-agent3/agent3/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> A system-level ambient AI voice agent that feels natural, fast, and close at hand.

**Status**: Public Alpha (`v0.1.x`)

## ✨ Built For Everyone

Even if you're not technical, getting started usually takes just one API key.

Agent3 is ready for early adopters, builders, and contributors who want to try a new kind of desktop voice agent. Core voice workflows already work, but the product is still evolving quickly.

📱 **Tech Stack**: Tauri 2.0 + Vue 3 + TypeScript + Rust + SQLite + cpal  
🧠 **Providers**: OpenAI, Gemini (Realtime WebSockets)  
🚀 **Features**: All-native audio processing, Rustpotter wakeword detection, transparent desktop overlay, local knowledge graph memory, and OS-level tool execution.

## ✅ What Works Today

- Realtime voice conversation with OpenAI and Gemini
- Native audio capture and playback in Rust
- Wake word creation, activation, and detection
- Voice switching, desktop overlay, and tray-based controls
- Local persistence for settings, transcripts, and memory data

## ⚠️ Current Limits

- This is not a polished 1.0 consumer release yet
- Behavior may still vary across devices, microphones, and OS edge cases
- Onboarding has improved, but some system-level flows may still need refinement
- Expect rapid iteration and occasional breaking changes

## 🔒 Privacy Notes

- Audio is captured locally by the native app
- Settings and memory data are stored locally in SQLite
- Realtime conversation audio and messages are sent to your configured AI provider when active
- You should review the privacy terms of the provider you choose

## ⚠️ Security Warning

- Agent3 can execute OS-level tools, including shell commands and desktop actions
- Do not use it on machines that contain highly sensitive data unless you fully trust the setup
- Review provider settings, prompts, and future tool permissions carefully before daily use
- For early testing, prefer a non-critical environment or secondary machine

## ⚡ Prerequisites

To build and run Agent3, you need the following installed:
- [Rust](https://www.rust-lang.org/tools/install) (latest stable)
- [Node.js](https://nodejs.org/) (v18+)
- [pnpm](https://pnpm.io/installation) (v8+)
- OS-specific build dependencies for Tauri (see [Tauri Prerequisites](https://tauri.app/v1/guides/getting-started/prerequisites))

## 🛠️ Quick Start

```bash
# 1. Install dependencies
pnpm install

# 2. Run in development mode (hot-reloading enabled)
pnpm tauri dev
```

> **Note**: The main window is a transparent, always-on-top click-through layer. To access configuration, look for the Agent3 icon in your system tray and open Settings.

## 🤝 Contributing

We welcome community contributions! Please read our [CONTRIBUTING.md](CONTRIBUTING.md) for details on our code of conduct, development workflow, and the process for submitting Pull Requests to us.
**Note**: All Pull Requests should target the `dev` branch.

## 📜 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
