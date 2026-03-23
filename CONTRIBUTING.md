# Contributing to Agent3

We love your input! We want to make contributing to this project as easy and transparent as possible, whether it's:

- Reporting a bug
- Discussing the current state of the code
- Submitting a fix
- Proposing new features
- Becoming a maintainer

## Development Setup

### Commands

Our frontend setup relies on Vite, and the backend on Tauri + Rust.
Always run frontend package commands at the root, and specific Rust checks inside `src-tauri/`.

```bash
pnpm install          # Install dependencies
pnpm tauri dev        # Full dev frontend + Rust backend
```

**Checking code:**
⚠️ Because we have a Tauri workspace, running plain `cargo check` at the root may not work as expected. 
Always run `cargo check` **inside** the `src-tauri/` directory:

```bash
cd src-tauri
cargo check
cargo clippy
```

### Gotchas / Pitfalls

- **Transparent App:** The main window consists of a transparent, always-on-top, click-through overlay. You will not see a standard window frame. Configuration is done via the **system tray** icon.
- **Port Locking:** The Vite dev server is locked to port `1420`.
- **Rust Rebuilds:** Rust rebuilds are managed by the Tauri CLI. Making changes to `src-tauri/**` will prompt Tauri to recompile, but Vite's standard web watcher doesn't track Rust files.

## Pull Requests

1. **Target Branch:** All Pull Requests **must** be opened against the `dev` branch. `main` is reserved for stable releases.
2. Fork the repo and create your branch from `dev`.
3. If you've added code that should be tested, add tests.
4. If you've changed APIs, update the documentation.
5. Ensure the test suite passes (if applicable) and your code lints (e.g., `cargo clippy`).
6. Issue that pull request!

## Code Style

- **General**: Use English for file/module names in `snake_case`. Comments and documentation must also be in English. Event names should be `kebab-case`.
- **Rust**: 
  - Use `.map_err(|e| e.to_string())?` for returning Tauri commands.
  - Import order: `std` → external crates → `crate::` internal.
- **Vue 3**: 
  - `<script setup lang="ts">` Composition API only.
  - Pure CSS + CSS variables (no Tailwind).

## Getting Help

For a deep dive into the architecture, multi-page windows, database migrations, and our memory model, please thoroughly read the `copilot-instructions.md` in `.github/`.
