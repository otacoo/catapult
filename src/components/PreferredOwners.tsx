import { useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ChevronUp,
  ChevronDown,
  X,
  Plus,
  Loader,
  RotateCcw,
} from "lucide-react";

interface Props {
  owners: string[];
  onChange: (owners: string[]) => void;
}

export const DEFAULT_PREFERRED_OWNERS = [
  "ggml-org",
  "bartowski",
  "unsloth",
  "AesSedai",
  "ubergarm",
  "mradermacher",
];

export default function PreferredOwners({ owners, onChange }: Props) {
  const [input, setInput] = useState("");
  const [validating, setValidating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const moveUp = (i: number) => {
    if (i === 0) return;
    const next = [...owners];
    [next[i - 1], next[i]] = [next[i], next[i - 1]];
    onChange(next);
  };

  const moveDown = (i: number) => {
    if (i >= owners.length - 1) return;
    const next = [...owners];
    [next[i], next[i + 1]] = [next[i + 1], next[i]];
    onChange(next);
  };

  const remove = (i: number) => {
    onChange(owners.filter((_, idx) => idx !== i));
  };

  const add = async () => {
    const name = input.trim();
    if (!name) return;
    if (owners.includes(name)) {
      setError("Already in list");
      return;
    }
    setError(null);
    setValidating(true);
    try {
      const valid = await invoke<boolean>("validate_hf_owner", { owner: name });
      if (valid) {
        onChange([...owners, name]);
        setInput("");
        setError(null);
      } else {
        setError("No GGUF models found for this user/org");
      }
    } catch (e) {
      setError("Could not validate — check your connection");
    } finally {
      setValidating(false);
    }
  };

  const resetDefaults = () => {
    onChange([...DEFAULT_PREFERRED_OWNERS]);
  };

  const isDefault =
    owners.length === DEFAULT_PREFERRED_OWNERS.length &&
    owners.every((o, i) => o === DEFAULT_PREFERRED_OWNERS[i]);

  return (
    <div>
      <div className="space-y-1">
        {owners.map((owner, i) => (
          <div
            key={`${owner}-${i}`}
            className="flex items-center gap-1.5 px-2 py-1.5 border border-border hover:border-border-strong transition-colors group"
          >
            <div className="flex flex-col gap-0.5">
              <button
                className="text-gray-600 hover:text-gray-300 disabled:opacity-20 disabled:cursor-default"
                onClick={() => moveUp(i)}
                disabled={i === 0}
                title="Move up"
              >
                <ChevronUp size={12} />
              </button>
              <button
                className="text-gray-600 hover:text-gray-300 disabled:opacity-20 disabled:cursor-default"
                onClick={() => moveDown(i)}
                disabled={i === owners.length - 1}
                title="Move down"
              >
                <ChevronDown size={12} />
              </button>
            </div>

            <span className="flex-1 text-sm font-mono text-gray-300">
              {owner}
            </span>

            <span className="text-[10px] text-gray-600 tabular-nums">
              #{i + 1}
            </span>

            <button
              className="text-gray-600 hover:text-accent-red opacity-0 group-hover:opacity-100 transition-opacity"
              onClick={() => remove(i)}
              title="Remove"
            >
              <X size={13} />
            </button>
          </div>
        ))}
        {owners.length === 0 && (
          <p className="text-xs text-gray-500 italic py-2">
            No preferred sources. The Browse tab owner filter will be empty.
          </p>
        )}
      </div>

      <div className="mt-3 flex gap-2 items-start">
        <div className="flex-1">
          <div className="flex gap-2">
            <input
              ref={inputRef}
              className="input flex-1 text-sm"
              placeholder="Add HuggingFace user/org…"
              value={input}
              onChange={(e) => {
                setInput(e.target.value);
                setError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !validating) add();
              }}
              disabled={validating}
            />
            <button
              className="btn-primary text-xs"
              onClick={add}
              disabled={validating || !input.trim()}
            >
              {validating ? (
                <Loader size={12} className="animate-spin" />
              ) : (
                <Plus size={12} />
              )}
              Add
            </button>
          </div>
          {error && (
            <p className="text-[10px] text-accent-red mt-1">{error}</p>
          )}
        </div>
      </div>

      {!isDefault && (
        <button
          className="btn-ghost text-xs mt-3"
          onClick={resetDefaults}
        >
          <RotateCcw size={11} /> Reset to defaults
        </button>
      )}
    </div>
  );
}
