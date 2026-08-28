// ── File tools (mirrors llama.cpp `--tools`) ────────────────────────────────

export interface ToolDef {
  name: string;
  label: string;
  hint: string;
  dangerous?: boolean;
}

// Only tools available across llama.cpp builds are listed here — some builds
// add extra tools (apply_diff, get_datetime) which are *not* offered because
// enabling an unknown tool makes the server fail to start.
export const KNOWN_TOOLS: ToolDef[] = [
  { name: "read_file", label: "Read File", hint: "Read text files (16 KB max per read)" },
  { name: "grep_search", label: "Grep Search", hint: "Search file contents with regex" },
  { name: "file_glob_search", label: "File Glob Search", hint: "List files matching a glob pattern" },
  { name: "get_info", label: "Get Info", hint: "Query file and folder metadata" },
  { name: "write_file", label: "Write File", hint: "Create or overwrite files" },
  { name: "edit_file", label: "Edit File", hint: "Apply line-range edits to files" },
  { name: "exec_shell_command", label: "Shell Command", hint: "Run arbitrary shell commands", dangerous: true },
];

// Effective set of enabled tools. "all" expands to every known tool; names not
// in KNOWN_TOOLS are dropped because passing an unknown tool name makes the
// server fail to start.
export function effectiveTools(value: string): Set<string> {
  const sel = new Set<string>();
  if (!value) return sel;
  if (value.trim().toLowerCase() === "all") {
    for (const t of KNOWN_TOOLS) sel.add(t.name);
    return sel;
  }
  for (const name of value.split(",")) {
    const n = name.trim();
    if (n && KNOWN_TOOLS.some((t) => t.name === n)) sel.add(n);
  }
  return sel;
}

// Canonical --tools argument for a value: "" (off), "all", or a sorted
// comma-separated list.
export function toolsArgValue(value: string): string {
  const sel = effectiveTools(value);
  if (sel.size === 0) return "";
  if (sel.size === KNOWN_TOOLS.length) return "all";
  return [...sel].sort().join(",");
}

// Drops tool names not supported by the running llama.cpp build from
// extra_params["tools"] so stale/session values can't fail server startup.
export function sanitizeTools(extra: Record<string, string>): Record<string, string> {
  if (!("tools" in extra)) return extra;
  const t = toolsArgValue(extra.tools);
  if (t) extra.tools = t; else delete extra.tools;
  return extra;
}