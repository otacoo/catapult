import { describe, test, expect } from "vitest";
import { DEFAULT_PREFERRED_OWNERS } from "./PreferredOwners";

describe("DEFAULT_PREFERRED_OWNERS", () => {
  test("is non-empty", () => {
    expect(DEFAULT_PREFERRED_OWNERS.length).toBeGreaterThan(0);
  });

  test("contains expected defaults", () => {
    expect(DEFAULT_PREFERRED_OWNERS).toContain("ggml-org");
    expect(DEFAULT_PREFERRED_OWNERS).toContain("bartowski");
    expect(DEFAULT_PREFERRED_OWNERS).toContain("unsloth");
    expect(DEFAULT_PREFERRED_OWNERS).toContain("AesSedai");
    expect(DEFAULT_PREFERRED_OWNERS).toContain("ubergarm");
    expect(DEFAULT_PREFERRED_OWNERS).toContain("mradermacher");
  });

  test("has no duplicates", () => {
    const unique = new Set(DEFAULT_PREFERRED_OWNERS);
    expect(unique.size).toBe(DEFAULT_PREFERRED_OWNERS.length);
  });

  test("entries are non-empty trimmed strings", () => {
    for (const owner of DEFAULT_PREFERRED_OWNERS) {
      expect(owner.length).toBeGreaterThan(0);
      expect(owner).toBe(owner.trim());
    }
  });

  test("ggml-org is first (highest priority)", () => {
    expect(DEFAULT_PREFERRED_OWNERS[0]).toBe("ggml-org");
  });
});

describe("preferred owners list operations", () => {
  const initial = ["ggml-org", "bartowski", "unsloth"];

  test("move up swaps with predecessor", () => {
    const list = [...initial];
    const i = 1;
    [list[i - 1], list[i]] = [list[i], list[i - 1]];
    expect(list).toEqual(["bartowski", "ggml-org", "unsloth"]);
  });

  test("move down swaps with successor", () => {
    const list = [...initial];
    const i = 1;
    [list[i], list[i + 1]] = [list[i + 1], list[i]];
    expect(list).toEqual(["ggml-org", "unsloth", "bartowski"]);
  });

  test("remove filters out by index", () => {
    const result = initial.filter((_, idx) => idx !== 1);
    expect(result).toEqual(["ggml-org", "unsloth"]);
  });

  test("remove last item yields empty list", () => {
    const single = ["ggml-org"];
    const result = single.filter((_, idx) => idx !== 0);
    expect(result).toEqual([]);
  });

  test("add appends to end", () => {
    const result = [...initial, "neworg"];
    expect(result).toEqual(["ggml-org", "bartowski", "unsloth", "neworg"]);
  });

  test("duplicate detection", () => {
    const list = [...initial];
    const name = "bartowski";
    expect(list.includes(name)).toBe(true);
  });

  test("duplicate detection is case-sensitive", () => {
    const list = [...initial];
    // HuggingFace usernames are case-sensitive
    expect(list.includes("Bartowski")).toBe(false);
  });

  test("move up at index 0 is a no-op", () => {
    const list = [...initial];
    const i = 0;
    if (i > 0) {
      [list[i - 1], list[i]] = [list[i], list[i - 1]];
    }
    expect(list).toEqual(initial);
  });

  test("move down at last index is a no-op", () => {
    const list = [...initial];
    const i = list.length - 1;
    if (i < list.length - 1) {
      [list[i], list[i + 1]] = [list[i + 1], list[i]];
    }
    expect(list).toEqual(initial);
  });
});
