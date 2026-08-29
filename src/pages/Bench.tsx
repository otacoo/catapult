import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Trash2, RefreshCw, FlaskConical } from "lucide-react";
import type { BenchResult } from "../types";

export default function Bench() {
  const [results, setResults] = useState<BenchResult[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await invoke<BenchResult[]>("list_bench_results");
      setResults(r.slice().reverse()); // newest first
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const clear = async () => {
    if (!window.confirm("Clear all bench history?")) return;
    try {
      await invoke("clear_bench_results");
      setResults([]);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="flex-1 overflow-y-auto p-6 space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold text-gray-100 flex items-center gap-2">
            <FlaskConical size={20} className="text-primary-light" /> Bench
          </h1>
          <p className="text-xs text-gray-500 mt-1">
            History of Quick Bench runs (<code className="font-mono">512+128</code> 1-rep, <code className="font-mono">bench_results.json</code>).
          </p>
          <p className="text-xs text-gray-600 mt-1">
            Tip: higher <code className="font-mono">tg</code> = faster generation, higher <code className="font-mono">pp</code> = faster prefill. Compare rows before/after changing a knob.
          </p>
        </div>
        <div className="flex gap-2">
          <button className="btn-ghost text-xs py-1 px-2" onClick={load} disabled={loading}>
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} /> Refresh
          </button>
          <button className="btn-ghost text-xs py-1 px-2 text-accent-red" onClick={clear} disabled={results.length === 0}>
            <Trash2 size={12} className="inline mr-1" />Clear
          </button>
        </div>
      </div>

      {error && (
        <div className="card border-accent-red/30 bg-accent-red/5 text-sm text-accent-red">{error}</div>
      )}

      {results.length === 0 ? (
        <div className="card">
          <p className="text-sm text-gray-500">No bench results yet. Run <em>Quick Bench</em> from <em>Run</em> to append here.</p>
          <p className="text-xs text-gray-600 mt-2">Each run is appended to <code className="font-mono">bench_results.json</code> (like <code className="font-mono">results.csv</code> in llama-optimize). Use it to compare before/after tweaking threads/batch/ngl.</p>
        </div>
      ) : (
        <div className="card p-0 overflow-hidden">
          <div className="overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="bg-surface-2 text-gray-500">
                <tr>
                  <th className="text-left px-3 py-2 font-medium">Time</th>
                  <th className="text-left px-3 py-2 font-medium">Model</th>
                  <th className="text-center px-3 py-2 font-medium">Build</th>
                  <th className="text-right px-3 py-2 font-medium">pp t/s</th>
                  <th className="text-right px-3 py-2 font-medium">tg t/s</th>
                  <th className="text-center px-3 py-2 font-medium">thr</th>
                  <th className="text-center px-3 py-2 font-medium">batch</th>
                  <th className="text-left px-3 py-2 font-medium">status</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {results.map((r, i) => (
                  <tr key={i} className="hover:bg-surface-3">
                    <td className="px-3 py-2 text-gray-500 font-mono whitespace-nowrap">{r.timestamp ? new Date(r.timestamp).toLocaleString() : "–"}</td>
                    <td className="px-3 py-2 text-gray-300 truncate max-w-[180px]" title={r.model_path}>{r.model_name || r.model_path.split(/[\\/]/).pop() || "–"}</td>
                    <td className="px-3 py-2 text-center text-gray-500 font-mono" title={r.build_commit || ""}>{r.build_number ?? "–"}</td>
                    <td className="px-3 py-2 text-right text-gray-200">{r.pp_tps?.toFixed(1) ?? "–"}</td>
                    <td className="px-3 py-2 text-right text-gray-200">{r.tg_tps?.toFixed(1) ?? "–"}</td>
                    <td className="px-3 py-2 text-center text-gray-500">{r.n_threads ?? "auto"}</td>
                    <td className="px-3 py-2 text-center text-gray-500">{r.batch_size}/{r.ubatch_size}</td>
                    <td className="px-3 py-2 text-gray-500 truncate max-w-[160px]" title={r.status}>{r.status}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
