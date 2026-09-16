import { describe, test, expect } from "vitest";
import { migrateExtraParams } from "./Server";

describe("migrateExtraParams", () => {
  test("drops removed ngram-size flags", () => {
    const out = migrateExtraParams({
      "spec-ngram-size-n": "3",
      "spec-ngram-size-m": "5",
      "spec-ngram-min-hits": "1",
      "kept": "1",
    });
    expect(out).toEqual({ kept: "1" });
  });

  test("renames removed --draft / --draft-min flags to spec-draft-*", () => {
    const out = migrateExtraParams({
      "draft": "16",
      "draft-min": "0",
    });
    expect(out).toEqual({ "spec-draft-n-max": "16", "spec-draft-n-min": "0" });
  });

  test("canonicalises draft aliases", () => {
    const out = migrateExtraParams({
      "model-draft": "/p/d.gguf",
      "n-gpu-layers-draft": "99",
      "threads-draft": "4",
      "device-draft": "cuda0",
      "cpu-moe-draft": "",
    });
    expect(out["spec-draft-model"]).toBe("/p/d.gguf");
    expect(out["spec-draft-ngl"]).toBe("99");
    expect(out["spec-draft-threads"]).toBe("4");
    expect(out["spec-draft-device"]).toBe("cuda0");
    expect(out["spec-draft-cpu-moe"]).toBe("");
    expect("model-draft" in out).toBe(false);
  });

  test("drops bogus draft ctx keys (never existed upstream)", () => {
    const out = migrateExtraParams({
      "ctx-size-draft": "4096",
      "spec-draft-ctx-size": "4096",
      "spec-draft-n-max": "7",
    });
    expect("ctx-size-draft" in out).toBe(false);
    expect("spec-draft-ctx-size" in out).toBe(false);
    expect(out["spec-draft-n-max"]).toBe("7");
  });

  test("drops flags that are not llama-server flags", () => {
    const out = migrateExtraParams({
      "embd-separator": "\\n",
      "cls-separator": "\\t",
      "model-vocoder": "/p/v.gguf",
      "tts-use-guide-tokens": "",
      "profile": "",
      "profile-output": "p.json",
      "verbose-prompt": "",
      "kept": "1",
    });
    expect(out).toEqual({ kept: "1" });
  });

  test("renames checkpoint-every-n-tokens to checkpoint-min-step", () => {
    const out = migrateExtraParams({ "checkpoint-every-n-tokens": "100" });
    expect(out).toEqual({ "checkpoint-min-step": "100" });
  });

  test("does not clobber existing canonical value", () => {
    const out = migrateExtraParams({
      "spec-draft-n-max": "32",
      "draft": "16",
    });
    expect(out["spec-draft-n-max"]).toBe("32");
    expect("draft" in out).toBe(false);
  });

  test("idempotent on a clean map", () => {
    const input = { "spec-default": "", "temp": "0.7" };
    const out = migrateExtraParams(input);
    expect(out).toEqual(input);
  });
});
