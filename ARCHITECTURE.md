# Architecture

Catapult is a Tauri v2 desktop application serving as a launcher for [llama.cpp](https://github.com/ggml-org/llama.cpp). It handles runtime version management, model discovery, server configuration, and provides an embedded chat interface.

## Directory structure

```
catapult/
├── src-tauri/src/           # Rust backend (~5,900 LOC)
│   ├── lib.rs               # Tauri command registration, AppState, IPC handlers
│   ├── config.rs            # AppConfig persistence, runtime/model types
│   ├── hardware.rs          # CPU/RAM/GPU detection, backend scoring, config suggestions
│   ├── runtime.rs           # GitHub release fetching, asset scoring, download/extraction
│   ├── models.rs            # GGUF scanning, metadata parsing, model download with resume
│   ├── server.rs            # ServerConfig, process spawn/kill, CLI arg builder, tool sanitizing
│   ├── mcp.rs               # MCP server storage (Cursor-compatible mcp.json)
│   ├── huggingface.rs       # HF API search, recommended models, quant extraction, presets.ini fetch
│   └── main.rs              # Entry point
├── src/                     # React/TypeScript frontend (~5,750 LOC)
│   ├── App.tsx              # Router (wizard + main layout)
│   ├── main.tsx             # React entry
│   ├── pages/
│   │   ├── Dashboard.tsx    # System overview, quick launch, favorite models
│   │   ├── Runtime.tsx      # Managed/custom runtime management, downloads
│   │   ├── Models.tsx       # Model browser, search, columnar list, directories
│   │   ├── Tools.tsx        # Tools/MCP: built-in file tool toggles + MCP server manager
│   │   ├── Server.tsx       # Two-column Run page: model, tabs+search, memory/logs
│   │   ├── Server.migrate.test.ts # extra_params migration tests
│   │   ├── Chat.tsx         # Embedded llama.cpp WebUI iframe
│   │   └── Wizard.tsx       # First-launch setup (runtime + model + appearance)
│   ├── components/
│   │   ├── Layout.tsx       # Sidebar navigation shell
│   │   └── CatapultIcon.tsx # SVG catapult icon
│   ├── types/index.ts       # TypeScript interfaces mirroring Rust structs
│   ├── utils/format.ts      # Shared formatting utilities
│   ├── utils/tools.ts       # Built-in tool set normalization/sanitization (mirrors server.rs)
│   └── styles/globals.css   # Tailwind component classes
└── tests
    ├── (Rust)               # Unit tests in #[cfg(test)] modules
    └── src/*/*.test.ts      # Vitest unit tests (format, tools, migration, components)
```

## IPC pattern

All filesystem, network, and process operations live in Rust. The frontend calls `invoke()` for request/response and `listen()` for streaming events. There are 55 registered Tauri commands spanning hardware detection, runtime management, model operations, server control, configuration, presets, per-model preset memory, file tools, and MCP servers.

**Events:**
- `download_progress` (DownloadProgress) — streamed during runtime and model downloads
- `server_log` (string) — each line of llama-server stdout/stderr

## Data directories

All paths are cross-platform via the `dirs` crate, relative to `dirs::data_dir()`:

```
{data_dir}/catapult/
├── config.json              # AppConfig (all settings)
├── gguf_cache.json          # GGUF metadata cache (path → name/params/ctx/vision)
├── runtimes/                # Managed runtime versions
│   ├── b5000-cuda/          # Versioned subdirectory per build+backend
│   └── b5100-cuda/
├── runtime/                 # Legacy single-runtime directory (migrated on load)
├── models/                  # Default model download directory
├── presets/                 # Server configuration presets (*.json)
│   └── __default__.json     # User-saved default settings
├── mcp.json                 # MCP server definitions (Cursor-compatible, `--mcp-servers-config`)
```

## Runtime management

Runtimes are either **managed** (downloaded from GitHub releases) or **custom** (user-pointed local installations).

### Managed runtimes
- Stored in versioned subdirectories: `runtimes/b{build}-{backend}/`
- Multiple versions can coexist; one is active at a time
- Old versions can be auto-deleted on new install (`auto_delete_old_runtimes` config flag)
- Non-active versions shown in a collapsible "archived" section
- Config tracks: build number, tag, backend ID/label, asset name, directory, install timestamp

### Custom runtimes
- Point to any directory containing a `llama-server` binary
- Scanning is recursive (depth 5) and detects multiple builds (e.g. `build/` + `vulkan/`)
- Multiple custom runtimes can be registered; one is active at a time

### Asset scoring
Each GitHub release asset is scored for the current platform: CUDA=100, Metal=95, ROCm=90, Vulkan=70, SYCL=60, CPU AVX-512=30, CPU AVX2=25, CPU AVX=20, CPU no-AVX=10. Backends not available on the system are penalized by -200.

## Model management

### Scanning
- Multiple GGUF storage directories can be configured, each scanned recursively (depth 5)
- A separate download directory is designated for new model downloads
- Imatrix/importance_matrix files are filtered from the display
- Split GGUF files (e.g. `model-00001-of-00003.gguf`) are consolidated into single logical model entries when all parts are present; incomplete sets show parts individually
- Deduplication by canonical path handles symlinks and overlapping directories
- `__downloading__` temp files are excluded from listings

### GGUF metadata
A binary parser reads GGUF v3 headers to extract:
- `general.name` — model name
- `general.size_label` — parameter count (e.g. "9.4B")
- `general.architecture` — used to locate context length key
- `{arch}.context_length` — training context window
- `general.tags` — string array; presence of "image-to-text" or "image-text-to-text" marks vision capability

Results are cached in `gguf_cache.json` keyed by file path, invalidated when file size or modification time changes. First scan reads headers; subsequent scans use cached data for near-instant loading.

### Vision models
Models tagged as vision-capable are paired with compatible mmproj files found in the same directory. Matching requires the mmproj filename to contain "mmproj" and share at least 2 name segments with the model (e.g. "Qwen3.5" + "4B"). The mmproj path is automatically passed as `--mmproj` when starting the server.

### Downloads
- HTTP Range resume support for interrupted downloads
- Exponential backoff retry: delays of 0s, 1s, 2s, 4s, 8s
- Consecutive failure counter resets when data is received (making flaky connections retry indefinitely as long as progress is made)
- After 5 consecutive failures: download pauses with Resume/Abort buttons
- Temp files (`__downloading__` prefix) preserved for resume across app restarts
- **Split/multipart models**: downloaded sequentially part-by-part with combined progress reporting; already-completed parts are skipped on resume; abort/delete cleans up all parts
- HuggingFace repo tree traversal is recursive (depth 3) to discover split models in subdirectories
- Active downloads are displayed in a persistent bar on the Models page regardless of active tab

## Server configuration

### ServerConfig
Core typed fields: model path, mmproj path, working directory, host, port, context size, GPU layers, threads, flash attention mode, KV cache types, sampling parameters (temperature, top-k/p, min-p, seed), batch sizes, memory flags (mlock, mmap), RoPE parameters, parallel slots.

The `working_dir` field sets the default working directory (`current_dir`) for the spawned `llama-server` process — the CWD its built-in tools operate in. It is a default directory, not a sandbox. Persisted app-wide via `AppConfig.server_working_dir` (`set_server_working_dir` command), seeded into the session when unset, and excluded from presets (per-session, like model path).

The Advanced tab covers an extended set of parameters including: MoE CPU offloading (`cpu-moe`, `n-cpu-moe`), weight repacking (`no-repack`), host tensor offload (`no-op-offload`), device bypass (`no-host`), memory auto-fitting (`--fit`, `--fit-margin`, `--fit-ctx`), KV unified buffer (`kv-unified`), N-gram speculation (`spec-ngram-size-n/m`, `spec-ngram-min-hits`), lookup cache files, draft model threading/device params, embedding/classification separators, WebUI config overrides, and `reuse-port`. Built-in tools (`--tools`) and MCP servers are configured app-wide on the Tools/MCP page, not per-run.

All additional llama-server parameters are stored in `extra_params: HashMap<String, String>` where:
- Keys are CLI flag names without `--` prefix (e.g. "api-key", "timeout")
- Empty values represent boolean flags (emitted as just `--flag`)
- Non-empty values are emitted as `--flag value`
- Special key `__raw__` holds free-form CLI arguments split by whitespace
- The `mmproj` key is filtered from extra_params (handled as a typed field)

**Built-in tools (app-wide)**: The Tools/MCP page (`/tools`) manages llama.cpp's built-in file tools via checkboxes limited to the tool set available across current builds (`read_file`, `file_glob_search`, `grep_search`, `exec_shell_command`, `write_file`, `edit_file`, `get_info`). The selection is stored app-wide in `AppConfig.server_tools` (`set_tools` command); `toolsArgValue` (frontend, `src/utils/tools.ts`) and `sanitize_tools`/`tool_arg_value` (backend, `server.rs`) mirror each other — empty means `--tools` is omitted, the full set collapses to `all`, otherwise a sorted CSV. On every `start_server`, `apply_global_tools` overrides whatever `tools` value a preset or session config carried (stale/unsupported names can't abort startup); the same sanitize runs again inside the arg builder. `exec_shell_command` requires an explicit confirmation before it can be enabled.

**MCP servers**: The Tools/MCP page also manages MCP (Model Context Protocol) servers persisted to `{data_dir}/catapult/mcp.json` in llama.cpp's Cursor-compatible shape — `{"mcpServers": { "<name>": { "command", "args", "env", "cwd", "timeout_ms" } }}` (see `mcp.rs`; `list_mcp_servers` / `save_mcp_servers` commands). When any servers are configured, `start_server` appends `--mcp-servers-config <path>` so llama-server spawns each stdio child and exposes its tools as `<server>_<tool>` in the WebUI. Entries without a command are dropped, matching llama.cpp's parsing. llama.cpp has no built-in web fetch/search tool — those arrive via an MCP server.

### Tabbed UI
Parameters are organized into 6 tabs: Context, Hardware, Sampling, Server, Chat, Advanced. The Advanced tab includes sub-sections for RoPE, speculative decoding, LoRA/control vectors, multimodal, CPU affinity, logging, the working directory, and a raw arguments text field. The built-in file tools formerly listed here now live on the app-wide Tools/MCP page.

### Run page layout
The Run page is a two-column split:
- **Top, full-width**: the model selector card with favorites, Auto-estimate, and the server controls (Launch/Stop, Open Chat).
- **Left column (fixed 450px)**: Memory Estimate visualizer and Server Logs. Logs flex-stretch to the window height (`flex-1 min-h-0`), long lines wrap (`whitespace-pre-wrap break-words`, no horizontal scrollbar), and new output auto-scrolls into view.
- **Right column (flexible)**: the tab bar, the settings search bar, and the active tab's config card. All six tabs stay mounted (`display:none` wrappers tagged with `data-tab`), which lets the search index settings across every tab via a DOM scan of `label`/`font-medium` toggle labels/`font-semibold` section titles; results jump to the field with a highlight.

### Presets
Server configurations are saved as JSON files in `{data_dir}/catapult/presets/`. A special `__default__` preset stores user-customized defaults. Model path, mmproj path, and working directory are excluded from presets (per-session); loading a preset preserves the current model selection, projector, and working directory.

**Per-model preset memory**: Each model can have a last-used preset associated with it. This association is stored in `AppConfig.model_presets` (`HashMap<String, String>`, keyed by model file path). When a model is selected, its saved preset is auto-loaded. When a preset is applied and a server is started, the model→preset association is persisted. Two new Tauri commands support this: `get_model_preset` and `set_model_preset`.

**HuggingFace `presets.ini` auto-import**: On successful model download, Catapult fetches `presets.ini` from the HF repo (if it exists) and saves it as a named preset (repo ID with `/` replaced by `__`). The file is parsed for sampling parameters: temperature, top-k/p, min-p, n-predict, seed, repeat-penalty, repeat-last-n. This is handled by `huggingface::fetch_presets_ini()` and `server::apply_hf_preset_params()`.

### Session persistence
Server configuration, active preset, and active tab are persisted to `sessionStorage` across page navigation within the same session. On initial load, state is restored from sessionStorage with fallback to saved defaults.

### Model selection (GUI)
- The model selector is a full-width card above the two-column split; the list is collapsible (shows the selected model name when collapsed)
- Models are sorted with favorites first; vision models display an eye icon
- Selecting a model checks for a saved preset (`get_model_preset`); if found, the preset is loaded instead of hardware suggestions. Otherwise, auto-applies suggested hardware settings (n_ctx, n_gpu_layers) without overriding user preferences

## Server process management

`start_server` spawns `llama-server` with `kill_on_drop(true)`. The child process is stored in `ServerState` (behind a Mutex). Stdout/stderr are read by independent tokio tasks (using manual `read_until` loops) that emit `server_log` events and buffer up to 500 lines. The full command line is stored as the first log entry.

Process exit is monitored by a polling task using `try_wait()` every 500ms. `stop_server` sends SIGTERM (Unix) or TerminateProcess (Windows), waits up to 30 seconds, then force-kills with SIGKILL if needed.

Status transitions: `Stopped → Starting → Running` (detected by "HTTP server listening" in output) or `Starting → Error` on process exit. On crash, error messages (exit code, process errors) are persisted to the log buffer and emitted as log events, ensuring error context is visible in the UI.

The frontend batches incoming log events via `requestAnimationFrame`, flushing accumulated lines once per frame to avoid performance issues with high-frequency output.

## First-launch wizard

A three-step onboarding flow at `/wizard` (outside the sidebar layout):
1. **System & Runtime** — hardware detection summary, runtime asset selection or custom directory browse, download with progress
2. **Model Selection** — recommended models filtered and sorted by hardware fit (VRAM/RAM), up to 3 selectable, parallel downloads
3. **Appearance** — theme choice (System / Dark / Light / Catapult) with live preview

Controlled by `wizard_completed` in AppConfig. Skippable at any time. Re-runnable via `--force-wizard` CLI flag or programmatic reset.

## Chat

The Chat page embeds llama.cpp's built-in WebUI in an `<iframe>` pointing at `http://127.0.0.1:{port}`. It is the single chat surface — "Open Chat" on the Run page navigates to it, and there is no separate chat window. The CSP in `tauri.conf.json` allows scripts, styles, connections, and WebSocket from `http://127.0.0.1:*` and `http://localhost:*` to support the embedded WebUI.

The iframe element is module-scoped in `Chat.tsx` and kept alive across route transitions, so the WebUI document (and its in-progress conversation) survives tab switching. The WebUI persists chat history and settings in the webview's `localStorage`, keyed by origin — which is `http://127.0.0.1:{port}`, so saved chats are per-port; changing the server port starts from a fresh origin.

## Styling
- Tailwind CSS with a dark theme (custom colors via `tailwind.config.js`)
- Sharp borders throughout (no border-radius on rectangular elements)
- Circular elements (status dots, toggle switches, radio buttons) retain `rounded-full`
- Component classes: `.card`, `.btn-*`, `.input`, `.badge-*`, `.progress-bar`
- Quantization badges use a color gradient by precision: blue (F16/Q8/Q7) → cyan (Q6) → green (Q5) → yellow (Q4) → orange (Q3) → red (Q2) → dark red (Q1). MXFP quants are mapped to equivalent Q levels.
- Custom catapult SVG icon in the sidebar

## Testing

- **Rust:** `cargo test` — 133 unit tests in `#[cfg(test)]` modules covering asset scoring, backend detection, CLI arg building, tool sanitization, MCP config round-tripping, quant extraction, size estimation, filename parsing, GGUF parsing, hardware config suggestions, split file parsing, imatrix detection, split model consolidation, `presets.ini` parsing, `apply_hf_preset_params`, preset name derivation, `AppConfig.model_presets` round-tripping, release-tag parsing, and config round-trips.
- **TypeScript:** `npm test` (Vitest) — 67 tests: formatting utilities for CPU/GPU name shortening, size formatting, quant color/sort mapping, imatrix detection, MXFP quant handling, PreferredOwners UI, `extra_params` migration, and tool-set normalization/sanitization.
- Tests caught a real bug: `noavx` backend detection was unreachable due to `contains("avx")` matching first
