# Catapult

A desktop launcher for [llama.cpp](https://github.com/ggml-org/llama.cpp). Manages runtime versions, discovers and downloads models, configures the server with full parameter coverage, and provides an embedded chat interface — all without touching the command line.

A Tauri v2 desktop application (Rust backend + React/TypeScript frontend):

<img width="1280" height="808" alt="catapult-ui" src="https://github.com/user-attachments/assets/a39fa2ae-d289-4bdd-a335-2d083666956c" />

## Features

**Runtime Management**
- Download managed llama.cpp builds from GitHub releases with automatic platform/backend detection
- CUDA version-aware selection: compares the installed toolkit against asset versions and penalizes mismatched builds
- Multiple versions can coexist; switch between them instantly
- Point to existing local llama.cpp installations (custom runtimes)
- Backend scoring: automatically recommends CUDA, Metal, ROCm, Vulkan, or CPU builds based on your hardware

**Model Management**
- Scan multiple local directories for GGUF models with recursive discovery
- Parse GGUF metadata (name, parameter count, context length, vision capability, layer count) directly from file headers
- Download models from HuggingFace with resume support and exponential backoff retry
- Curated list of recommended models filtered by your hardware
- HuggingFace browsing with owner filtering and sorting (by downloads, stars, or newest)
- Favorites, sorting, filtering, and quant-level color coding
- Vision model detection with automatic mmproj file pairing; vision models marked with an eye icon

**Server Configuration**
- Full llama.cpp server parameter coverage in a tabbed UI (Context, Hardware, Sampling, Server, Chat, Advanced)
- **Memory estimate**: live visual breakdown of VRAM/RAM usage (model weights, KV cache, overhead) as you adjust settings, with GQA-aware KV cache sizing
- **Auto-estimate**: one click suggests GPU offload, context size, and cache types that fit your hardware
- **`--fit` support**: let llama-server automatically fit context and GPU layers to device memory
- Context size slider (or auto = model default)
- Save and load named configuration presets; per-model preset memory (last-used preset auto-loads on model selection)
- Auto-import `presets.ini` from HuggingFace repos on model download (sampling parameters applied as a named preset)
- Process lifecycle management with log streaming and server logs view
- One-click launch from the dashboard

**Chat**
- Embedded llama.cpp WebUI in-app via iframe (conversation persists when switching tabs)
- Pop-out to a separate window

**API**
- Dedicated API tab showing live connection details for the running server: OpenAI-compatible base URL, endpoints, model ID, context, slots, and API key — all copyable
- Ready-to-paste client configuration (environment variables) for OpenAI SDKs

**First-Launch Wizard**
- Hardware detection and runtime recommendation
- Model selection with hardware fit indicators
- Get from zero to chatting in under a minute

**App Updates**
- Optional "Check for updates on app start" toggle and manual check button on the Dashboard (disabled by default — no automatic network calls)

## Download

Pre-built binaries for Linux, macOS (Universal), and Windows are available on the [Releases](../../releases) page.

| Platform | Format |
|----------|--------|
| Linux    | AppImage, .deb |
| macOS    | .dmg (Universal: Intel + Apple Silicon) |
| Windows  | .msi |

## Building from Source

### Prerequisites

- [Node.js](https://nodejs.org/) 18+
- [Rust](https://www.rust-lang.org/tools/install) (stable)
- Platform-specific dependencies (see below)

#### Linux

```bash
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev libappindicator3-dev librsvg2-dev patchelf
```

#### macOS / Windows

No additional system dependencies required.

### Build

```bash
# Install frontend dependencies
npm install

# Development mode (opens Tauri window with hot-reload)
npm run dev

# Production build (outputs to src-tauri/target/release/bundle/)
npm run build
```

## Testing

```bash
# Frontend tests (Vitest)
npm test

# Rust tests
cargo test --manifest-path src-tauri/Cargo.toml

# Type-check frontend
npx tsc --noEmit
```

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for a list of notable changes between releases.

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for detailed technical documentation covering the IPC pattern, data directories, runtime/model/server subsystems, and more.

## Tech Stack

- **Backend:** Rust, Tauri v2, Tokio, Reqwest, Serde
- **Frontend:** React, TypeScript, Vite, Tailwind CSS
- **Testing:** Vitest (frontend), `#[cfg(test)]` modules (backend)
- **CI:** GitHub Actions — tests on every push/PR, cross-platform builds on main/tags

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
