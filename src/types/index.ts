// ── Hardware ──────────────────────────────────────────────────────────────────

export interface SystemInfo {
  cpu_name: string;
  cpu_cores: number;
  cpu_threads: number;
  total_ram_mb: number;
  available_ram_mb: number;
  gpus: GpuInfo[];
  os: string;
  arch: string;
  available_backends: BackendInfo[];
  recommended_backend: string;
}

export interface GpuInfo {
  name: string;
  vram_mb: number;
  vendor: "nvidia" | "amd" | "intel" | "apple" | "unknown";
}

export interface BackendInfo {
  id: string;
  name: string;
  available: boolean;
  description: string;
  version?: string;
}

export interface SuggestedConfig {
  n_gpu_layers: number;
  n_ctx: number;
  can_fit_fully_in_vram: boolean;
  total_usable_mb: number;
  notes: string[];
  n_threads?: number | null;
  n_batch?: number | null;
  n_ubatch?: number | null;
}

export interface MemoryEstimate {
  model_mb: number;
  kv_cache_mb: number;
  overhead_mb: number;
  total_mb: number;
  vram_total_mb: number;
  ram_available_mb: number;
  vram_used_mb: number;
  ram_used_mb: number;
  vram_model_mb: number;
  vram_kv_mb: number;
  vram_overhead_mb: number;
  ram_model_mb: number;
  ram_kv_mb: number;
  ram_overhead_mb: number;
  fits: boolean;
  notes: string[];
}

export interface BenchResult {
  model_path: string;
  n_prompt: number;
  n_gen: number;
  n_threads: number | null;
  batch_size: number;
  ubatch_size: number;
  n_ctx: number;
  n_gpu_layers: number;
  pp_tps: number | null;
  tg_tps: number | null;
  status: string;
  raw_stdout: string;
  raw_stderr: string;
  timestamp: string;
  model_name: string;
  build_number?: number | null;
  build_commit?: string | null;
}

// ── Runtime ───────────────────────────────────────────────────────────────────

export interface RuntimeInfo {
  installed: boolean;
  build: number | null;
  backend: string | null;
  path: string | null;
  server_binary: string | null;
  runtime_type: "managed" | "custom" | "none";
}

export interface ManagedRuntimeInfo {
  build: number;
  tag_name: string;
  backend_id: string;
  backend_label: string;
  asset_name: string;
  dir_name: string;
  installed_at: number;
}

export interface CustomRuntimeInfo {
  label: string;
  binary_path: string;
}

export interface ReleaseInfo {
  tag_name: string;
  build: number;
  published_at: string;
  available_assets: AssetOption[];
}

export interface AssetOption {
  name: string;
  backend_id: string;
  backend_label: string;
  platform: string;
  download_url: string;
  size_mb: number;
  score: number;
}

export interface CustomBuild {
  binary_path: string;
  label: string;
}

export interface ScanResult {
  builds: CustomBuild[];
  is_source_distribution: boolean;
}

// ── Models ────────────────────────────────────────────────────────────────────

export interface ModelInfo {
  id: string;
  name: string;
  repo_id: string;
  filename: string;
  path: string;
  size_bytes: number;
  quant: string | null;
  params_b: string | null;
  context_length: number | null;
  is_vision: boolean;
  /** Reasoning-capable model (tags/capabilities or `<think>` chat template). */
  is_reasoning: boolean;
  mmproj_path: string | null;
  split_files: string[];
}

export interface RecommendedModel {
  repo_id: string;
  filename: string;
  name: string;
  description: string;
  params_b: number;
  family: string;
  quant: string;
  context: number | null;
  estimated_size_mb: number;
  installed: boolean;
  installed_path: string | null;
}

export interface HfModel {
  repo_id: string;
  name: string;
  author: string;
  tags: string[];
  files: HfFile[];
  downloads: number;
  likes: number;
}

export interface HfFile {
  filename: string;
  size_bytes: number;
  quant: string | null;
  download_url: string;
  is_split: boolean;
  split_parts: HfFilePart[];
  is_mmproj: boolean;
  /** DeepSeek V4 DSpark speculative-draft file (auxiliary, hidden in UI). */
  is_dspark: boolean;
}

export interface HfFilePart {
  filename: string;
  size_bytes: number;
  download_url: string;
}

export interface KnownOwner {
  id: string;
  description: string;
}

// ── Server ────────────────────────────────────────────────────────────────────

export interface ServerConfig {
  model_path: string;
  mmproj_path: string | null;
  working_dir: string | null;
  host: string;
  port: number;
  n_ctx: number;
  n_gpu_layers: number;
  n_threads: number | null;
  flash_attn: string;
  cache_type_k: string;
  cache_type_v: string;
  temperature: number;
  top_k: number;
  min_p: number;
  top_p: number;
  n_predict: number;
  /** Omit all sampling flags so the server/harness defaults apply. */
  disable_sampling: boolean;
  n_batch: number;
  n_ubatch: number;
  cont_batching: boolean;
  mlock: boolean;
  no_mmap: boolean;
  seed: number | null;
  rope_freq_scale: number | null;
  rope_freq_base: number | null;
  grp_attn_n: number | null;
  grp_attn_w: number | null;
  parallel: number;
  extra_params: Record<string, string>;
}

export type ServerStatus =
  | { type: "stopped" }
  | { type: "starting" }
  | { type: "running"; port: number; pid: number }
  | { type: "error"; message: string };

export interface ServerInfo {
  base_url: string;
  model_id: string;
  model_alias: string;
  model_path: string;
  n_ctx: number;
  n_predict: number;
  total_slots: number;
  slots_idle: number;
  api_key: string | null;
}

// ── Downloads ─────────────────────────────────────────────────────────────────

export interface DownloadProgress {
  id: string;
  bytes_downloaded: number;
  total_bytes: number;
  percent: number;
  status: string; // "downloading" | "extracting" | "done" | "error" | "paused" | "retrying (N/3)"
}

// ── Config ────────────────────────────────────────────────────────────────────

export interface AppConfig {
  managed_runtimes: ManagedRuntimeInfo[];
  custom_runtimes: CustomRuntimeInfo[];
  active_runtime: { type: "managed"; build: number; backend_id: string } | { type: "custom"; index: number } | { type: "none" };
  auto_delete_old_runtimes: boolean;
  models_dir: string | null;
  model_dirs: string[];
  download_dir: string | null;
  last_update_check: number | null;
  latest_known_build: number | null;
  auto_check_updates: boolean;
  favorite_models: string[];
  selected_model: string | null;
  wizard_completed: boolean;
  /** Maps model file path → last-used preset name */
  model_presets: Record<string, string>;
  preferred_owners: string[];
  server_working_dir: string | null;
  theme: AppTheme;
  /** App-wide selection of built-in file tools (`--tools`), managed on the Tools/MCP page. */
  server_tools: string[];
  /** Names of MCP servers toggled off on the Tools/MCP page. */
  mcp_disabled: string[];
  /** Last preset selected in Run tab, for restore on next launch. */
  last_preset: string | null;
  /** Show in notification area; close hides to tray instead of quitting. */
  close_to_tray: boolean;
  /** Enable Quick Bench (Run header button/card and Bench tab). */
  enable_quick_bench: boolean;
  /** Model paths registered for llama-server router mode (multi-model). */
  router_models: string[];
  /** Chat implementation: agent harness (default) or llama-server WebUI. */
  harness_chat: boolean;
  /** Turn budget for the agent orchestrator loop. */
  harness_max_turns: number;
  /** Turn budget for each ephemeral subagent run. */
  harness_subagent_max_turns: number;
  /** Model roles (requires router mode): which model plans vs. executes. */
  harness_roles: { orchestrator: string | null; worker: string | null };
  /** Chat projects: contained working directories shown in the Chat sidebar. */
  harness_projects: { id: string; name: string; path: string; created: number }[];
  /** Id of the project the Chat view currently has open. */
  harness_active_project: string | null;
}

/** Agent run result: text plus the metadata shown under each response. */
export interface HarnessRunResult {
  text: string;
  model?: string;
  tokens_per_sec?: number | null;
  gen_tokens: number;
  prompt_tokens?: number | null;
  elapsed_ms: number;
}

/** Model capability badges for the chat input row. */
export interface HarnessCapabilities {
  vision: boolean;
  reasoning: boolean;
  context_length?: number | null;
}

/** A file/image attachment chip in the chat input. */
export interface ChatAttachment {
  name: string;
  kind: "image" | "text";
  /** Full original path (re-read when sending). */
  path: string;
  /** For images: data URL for the chip preview. */
  preview?: string;
  /** For text attachments: file content (read at add time). */
  text?: string;
}

/** Sidebar session entry. */
export interface SessionInfo {
  id: string;
  title: string;
  updated: number;
  project?: string | null;
}

/** UI theme preference: "system" follows the OS light/dark setting. */
export type AppTheme = "system" | "dark" | "light" | "catapult";

// ── Server tools (live from /tools) ─────────────────────────────────────────

export interface ServerToolInfo {
  name: string;
  display_name: string;
  type: string; // "server" | "mcp"
  description: string;
}

// ── MCP servers ────────────────────────────────────────────────────────────────

/** One MCP server as shown/edited on the Tools/MCP page (mirrors `mcp.rs`). */
export interface McpServerEntry {
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  cwd: string | null;
  timeout_ms: number | null;
  /** App-side toggle; disabled names are stored in AppConfig.mcp_disabled. */
  enabled: boolean;
}

/** Response of `list_mcp_servers`: the servers plus the on-disk config path. */
export interface McpInfo {
  path: string;
  servers: McpServerEntry[];
}
