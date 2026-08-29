# Changelog

## [0.3.0] - 2026-08-29

### Added

- Live Server Tools probe in Tools tab and dedicated Bench tab with `bench_results.json` history for Quick Bench runs.
- Quick Bench button in Run header runs 1-rep `llama-bench` (512+128) with current threads/batch and shows pp/tg.
- Performance heuristics for threads, batch and micro-batch via Auto-estimate and workload profiles (single/agents/multi).
- Poll and tensor offload (`ot`) controls in Advanced Performance section.
- Updated curated models to 10 newer releases including LFM2.5 2.6B, Ornith 9B and Qwen3.6 35B.
- Build number column in Bench history.

### Changed

- Moved live MCP status from Chat to Tools to maximize chat space.
- Simplified Tool guidance text and moved Add MCP Server button to header.
- Added Open file and Show in folder actions for `mcp.json`.
- N CPU MoE hint now includes guide for 12GB cards.

### Fixed

- Windows MCP `npx` shims now auto-wrapped via `cmd /c` with case-insensitive PATHEXT handling.
- Bench no longer passes invalid `-c` and correctly parses `llama-bench` CSV/Markdown output.
- Bench results now mirrored to Server Logs for debugging.
- CI dead_code warnings for MCP helpers on Linux.

## [0.2.0] - 2026-08-28

### Added

- **Tools/MCP page**: A new tab between Models and Run. It manages app-wide built-in file tools plus MCP servers. MCP servers are persisted to `{data_dir}/catapult/mcp.json` (Cursor-compatible shape) and attached to every run via `--mcp-servers-config`; their tools appear in the chat UI as `<server>_<tool>` (e.g. a web fetch/search server).
- **Working directory option**: Server launches now run `llama-server` with a configurable working directory — the CWD your model's built-in tools (`read_file`, `file_glob_search`, `grep_search`, …) operate in. Stored per-app in `AppConfig` and auto-applied on load, it can be overridden per session from the Advanced tab. It's a default directory, not a sandbox: write-access prompts are still handled by llama.cpp's WebUI.
- **Settings search bar**: A search box under the Run page tab bar finds any setting across all six tabs (Context, Hardware, Sampling, Server, Chat, Advanced), jumps to the match with a highlight, and supports keyboard navigation (arrows + Enter, Escape to close).
- **Tool gating tests**: 14 unit tests for the tool set normalization, argument collapsing, and sanitization (`src/utils/tools.test.ts`).

### Changed

- **Run page layout**: Two-column layout — the model selector sits above both columns, server configuration tabs occupy the right (larger) column, and Memory Estimate + Server Logs live in a fixed left column. Logs stretch to the window height and long lines now wrap instead of scrolling horizontally.
- **Single embedded chat**: Removed the separate "Pop out" chat window. "Open Chat" now navigates to the embedded Chat tab, giving one WebUI instance instead of two independent browser contexts. Chat history and settings persist in the webview's localStorage per `host:port` — changing the server port starts a fresh origin.
- **File tools moved to Tools page**: The built-in tool picker left the Run page's Advanced tab. Tool selection is now app-wide (`AppConfig.server_tools`) and overrides any preset/session value on every start, so stale tool names can never abort llama-server.

### Fixed

- **`tools setup failed: unknown tool` on server start**: Stored tool lists that included build-specific tools (`get_datetime`, `apply_diff`) made llama-server exit(1). Tool names are now restricted to the stable set and stale values are sanitized whenever a session, preset, or default config is loaded.
- **"Cannot parse build number" on runtime updates**: GitHub release listing now fetches enough entries and tolerates semver-style (`v0.2.x`) release tags, so newer llama.cpp builds are detected and installed correctly.

## [0.1.6] - 2026-08-14

### Added

- **API tab**: Shows live connection details for the running server — OpenAI-compatible base URL, chat/completions/embeddings endpoints, model ID, alias, path, context size, slots, and API key — all copyable, plus a ready-to-paste client configuration (environment variables) for OpenAI SDKs.
- **Memory estimate**: The Run page now shows a live visual breakdown of estimated VRAM and RAM usage (model weights / KV cache / overhead) as bars against your hardware, updating as settings change. KV cache sizing accounts for GQA head counts, cache dtypes, and model context.
- **Auto-estimate button**: One click suggests GPU offload, context size, and cache types that fit your hardware. Context fitting uses the model's real layer/embedding metadata and sizes the KV cache to the VRAM left after offloaded weights.
- **`--fit` support**: The Fit toggle (Hardware tab) is now wired up — when on, `--fit on` is passed and `--ctx-size`/`--n-gpu-layers` are left to llama-server so it auto-fits context and layers to device memory.
- **App update settings**: New "App Updates" section on the Dashboard with a "Check for updates on app start" toggle and a manual "Check now" button. Disabled by default — no automatic update checks.
- **HuggingFace sort options**: Browse tab gained sort dropdown (By downloads / By stars / Newest) next to the owner filter.
- **CUDA version detection**: Runtime asset selection now prefers `nvcc --version` (falling back to `nvidia-smi`) and penalizes assets whose CUDA version doesn't match the installed toolkit, so a mismatched build never gets the "Recommended" badge. New `cuda-X.Y` asset naming is also parsed.
- **Wizard window controls**: The first-launch wizard now has a drag region and minimize/maximize/close buttons.
- **Context size slider**: The Context tab offers a slider from 512 to the model's max context, with an "Override default" toggle (default stays auto).
- **Shared Toggle component**: Extracted the switch control used across pages.

### Removed

- **TUI**: Removed the terminal interface (`catapult-tui` binary) and its dependencies. Catapult is now GUI-only.

### Fixed

- **Server stuck on "Starting…" forever**: Readiness detection only matched "HTTP server listening" / "server is listening"; newer llama.cpp logs "listening on http://…", which is now also matched.
- **Chat conversation resetting on tab switch**: The embedded WebUI iframe is now kept alive (module-scoped element) and re-attached instead of being recreated on every visit.
- **mmproj handling**: Turning "mmproj Auto" off no longer injects `--no-mmproj`; it simply stops auto-selecting a projector. The old "mmproj URL" field (which rejected local paths) is now "mmproj Path" and passes `--mmproj` with a local file path.
- **Memory estimate massively overshooting**: KV cache was computed with the full embedding dimension (ignoring grouped-query attention) and scaled with `--parallel`; both are now correct, cutting estimates by up to ~8× on GQA models.
- **Auto-update check on startup**: Removed the automatic updater check that fired on app start (and on the Runtime tab); checks now only happen when the user asks.
- **Server logs**: Moved below the Memory Estimate card and made selectable for copying.

## [0.1.5] - 2026-05-18

### Fixed

- **Added MTP support**: added support for MTP parameters

## [0.1.4] - 2026-05-05

### Fixed

- **Updated server parameters**: Updated parameters to match current `master` for llama.cpp

- **Bump versions and synchronize**: Bumped and synchronized Tauri versions

## [0.1.3] - 2026-04-16

### Fixed

- **macOS app unresponsive (issue #8)**: The debounce introduced in 0.1.2 did not fully resolve the issue. The root cause was the initial `isMaximized()` call on mount, which on macOS triggers a resize event, which calls `isMaximized()` again — an infinite loop. Removed the initial call entirely; the debounced resize handler already keeps the maximize indicator in sync.

- **TUI crash in logs tab (issue #13)**: After restarting the server, the new log file is shorter than the previous one. If the scroll position was beyond the end of the new log, the slice operation panicked with an out-of-range index. The scroll offset is now clamped to the new line count on every tick, with an additional guard in the render path.

## [0.1.2] - 2026-04-13

### Fixed

- **macOS app unresponsive (issue #8)**: Calling `isMaximized()` inside the window resize handler triggered an infinite resize event loop on macOS, freezing the entire UI. The check is now debounced so the loop cannot form.

- **`--parallel 1` not emitted (issue #11)**: The `--parallel` flag was only emitted when the value was greater than 1. Since llama.cpp defaults to 4 parallel slots when the flag is omitted, users could not explicitly request single-slot mode from the UI. The flag is now always emitted.

- **`--no-cont-batching` not emitted (issue #11)**: Disabling continuous batching in the UI had no effect — the `--no-cont-batching` flag was never passed to llama-server. It is now emitted when the toggle is off.

- **Virtual GPU selected over real GPU on Windows (issue #9)**: GPU detection via WMI returned all video adapters in arbitrary order, so virtual adapters (Hyper-V, Microsoft Basic Display, VMware, etc.) could be picked as the primary GPU. Virtual adapters are now filtered out when a real GPU is present.

- **Server process orphaned on GUI exit (issue #7)**: Closing the GUI window without stopping the server left llama-server running in the background with no way to reattach. A shutdown handler now terminates the server process when the app exits.

- **Zombie server processes in TUI (issue #7)**: Stopped llama-server processes lingered as zombies in the process table until the TUI itself exited. The child process handle is now properly dropped instead of leaked via `mem::forget`, and `waitpid` is called after the process is confirmed dead.

- **Console windows flashing on Windows (issue #10)**: Every child process spawned for hardware detection (PowerShell, nvidia-smi, etc.) opened a visible console window. All subprocess invocations now use `CREATE_NO_WINDOW` to suppress them.

## [0.1.1] - 2026-04-10

### Fixed

- **App icon**: Replaced placeholder purple square icons with a proper catapult icon across all platforms (PNG, ICO, ICNS) and added the missing web favicon SVG.

- **Per-backend runtime management**: Managed runtimes are now identified by both build number and backend (e.g., CUDA, Vulkan, ROCm). Previously, only the build number was used, which prevented users from installing and switching between multiple backends of the same build version. Downloading a new backend no longer removes existing backends for the same build. Auto-delete of old runtimes now only removes outdated versions of the same backend type.

- **mmproj download filename**: When downloading a vision projection (mmproj) file alongside a core model, the mmproj filename is now prefixed with the core model's base name (e.g., `Qwen2.5-VL-7B-mmproj-f16.gguf` instead of just `mmproj-f16.gguf`). This ensures the mmproj is correctly detected and paired with its companion model.

- **mmproj detection via GGUF metadata**: Vision projection files are now detected not only by filename (containing "mmproj") but also by GGUF metadata (`general.architecture == "clip"`). This fixes detection for mmproj files from repositories that don't include "mmproj" in the filename. Detected mmproj files are also excluded from the main installed models list.

- **Config erasure on runtime download**: The runtime download handler previously cloned the config before the async operation and wrote it back after completion, which could silently discard any concurrent config changes (e.g., model selection, preset saves) made while the download was in progress. The download now returns a structured result that is applied atomically to the live config under its mutex lock.

- **Config robustness**: If the config file fails to parse on startup, Catapult now backs it up to `config.json.bak` before falling back to defaults, preserving the original data for recovery. The `auto_check_updates` setting now correctly defaults to `true` for new installs (previously it could silently default to `false` if the field was absent from the JSON).

### Added

- **Custom runtime: source distribution auto-import**: When browsing for a custom runtime, Catapult now detects llama.cpp source distributions by the presence of `CMakeLists.txt`. All `llama-server` binaries found under the tree are automatically registered as individual custom runtime entries, making it easy to switch between build configurations (e.g., CUDA vs. Vulkan builds) from a local build tree.

- **One-click runtime update**: The "Update available" banner on the Runtime page now triggers the download inline and displays a progress bar in place, instead of redirecting to the releases browser. The releases browser remains available for manual version selection.

- **Scanning spinner**: A loading overlay is displayed while Catapult scans a selected directory for `llama-server` binaries, providing feedback for large source trees that take a moment to traverse.

## [0.1.0] - Initial release

First public release of Catapult, a GUI/TUI launcher for llama.cpp.
