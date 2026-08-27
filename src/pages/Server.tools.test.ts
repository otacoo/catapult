import { describe, test, expect } from "vitest";
import { effectiveTools, toolsArgValue, sanitizeTools } from "./Server";

const ALL_TOOLS = [
  "edit_file",
  "exec_shell_command",
  "file_glob_search",
  "get_info",
  "grep_search",
  "read_file",
  "write_file",
];

describe("effectiveTools", () => {
  test("empty value means no tools", () => {
    expect(effectiveTools("").size).toBe(0);
  });

  test("'all' expands to every known tool", () => {
    const sel = effectiveTools("all");
    expect(sel.size).toBe(ALL_TOOLS.length);
    for (const t of ALL_TOOLS) expect(sel.has(t)).toBe(true);
  });

  test("parses comma-separated list", () => {
    const sel = effectiveTools("read_file,write_file");
    expect(sel.has("read_file")).toBe(true);
    expect(sel.has("write_file")).toBe(true);
    expect(sel.has("grep_search")).toBe(false);
  });

  test("drops unknown tool names", () => {
    const sel = effectiveTools("read_file,future_tool");
    expect(sel.has("read_file")).toBe(true);
    expect(sel.has("future_tool")).toBe(false);
  });

  test("ignores empty segments", () => {
    expect(effectiveTools("read_file,,write_file").size).toBe(2);
  });
});

describe("toolsArgValue", () => {
  test("empty set produces empty argument", () => {
    expect(toolsArgValue("")).toBe("");
  });

  test("full known set collapses to 'all'", () => {
    expect(toolsArgValue("all")).toBe("all");
    expect(toolsArgValue(ALL_TOOLS.join(","))).toBe("all");
  });

  test("subset is sorted and comma-joined", () => {
    expect(toolsArgValue("write_file,read_file")).toBe("read_file,write_file");
  });

  test("unknown tools are dropped, not emitted", () => {
    expect(toolsArgValue("read_file,zz_new")).toBe("read_file");
  });

  test("unchecking one tool from 'all' yields the remaining list", () => {
    const withoutWrite = [...effectiveTools("all")].filter((n) => n !== "write_file");
    const expected = [...withoutWrite].sort().join(",");
    expect(toolsArgValue(withoutWrite.join(","))).toBe(expected);
    expect(toolsArgValue(withoutWrite.join(","))).not.toBe("all");
  });
});

describe("sanitizeTools", () => {
  test("strips tools unsupported by the running build", () => {
    const out = sanitizeTools({ tools: "file_glob_search,get_datetime,get_info" });
    expect(out).toEqual({ tools: "file_glob_search,get_info" });
  });

  test("removes the flag entirely when no known tools remain", () => {
    const out = sanitizeTools({ tools: "get_datetime,zz_new" });
    expect(out).toEqual({});
  });

  test("canonicalises and collapses a full set to 'all'", () => {
    const out = sanitizeTools({ tools: ALL_TOOLS.join(",") });
    expect(out).toEqual({ tools: "all" });
  });

  test("leaves extra_params without a tools key untouched", () => {
    const extra = { temp: "0.7" };
    expect(sanitizeTools(extra)).toEqual({ temp: "0.7" });
  });
});