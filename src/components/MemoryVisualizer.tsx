import type { MemoryEstimate } from "../types";

function bar(
  label: string,
  total: number,
  segments: { mb: number; className: string }[],
  used: number
) {
  const pct = (v: number) => (total > 0 ? Math.min(100, (v / total) * 100) : 0);
  const overflows = used > total;
  let cumulative = 0;
  return (
    <div>
      <div className="flex items-center justify-between mb-1">
        <span className="text-xs text-gray-400">{label}</span>
        <span className={`text-xs font-mono ${overflows ? "text-accent-red" : "text-gray-500"}`}>
          {(used / 1024).toFixed(2)} GB / {(total / 1024).toFixed(1)} GB
        </span>
      </div>
      <div className="relative h-5 bg-surface-4 border border-border overflow-hidden">
        {segments.map((s, i) => {
          const left = cumulative;
          cumulative += s.mb;
          return (
            <div
              key={i}
              className={`absolute top-0 bottom-0 ${s.className}`}
              style={{ left: `${pct(left)}%`, width: `${pct(s.mb)}%` }}
            />
          );
        })}
        {overflows && (
          <div className="absolute top-0 bottom-0 left-0 right-0 border-2 border-accent-red/70" />
        )}
      </div>
    </div>
  );
}

export default function MemoryVisualizer({ estimate }: { estimate: MemoryEstimate | null }) {
  if (!estimate) return null;

  return (
    <div className="space-y-3">
      <div className="flex items-center gap-4 text-[10px] text-gray-500">
        <span className="flex items-center gap-1"><span className="w-2.5 h-2.5 bg-accent-blue/70 inline-block" /> Model</span>
        <span className="flex items-center gap-1"><span className="w-2.5 h-2.5 bg-primary/70 inline-block" /> KV cache</span>
        <span className="flex items-center gap-1"><span className="w-2.5 h-2.5 bg-gray-600/70 inline-block" /> Overhead</span>
        <span className="ml-auto">{estimate.total_mb >= 1024 ? `${(estimate.total_mb / 1024).toFixed(2)} GB` : `${estimate.total_mb} MB`} estimated total</span>
      </div>
      {estimate.vram_total_mb > 0 && (
        bar("GPU VRAM", estimate.vram_total_mb, [
          { mb: estimate.vram_model_mb, className: "bg-accent-blue/70" },
          { mb: estimate.vram_kv_mb, className: "bg-primary/70" },
          { mb: estimate.vram_overhead_mb, className: "bg-gray-600/70" },
        ], estimate.vram_used_mb)
      )}
      {bar("System RAM", estimate.ram_available_mb, [
        { mb: estimate.ram_model_mb, className: "bg-accent-blue/70" },
        { mb: estimate.ram_kv_mb, className: "bg-primary/70" },
        { mb: estimate.ram_overhead_mb, className: "bg-gray-600/70" },
      ], estimate.ram_used_mb)}
      {estimate.notes.length > 0 && (
        <ul className="space-y-0.5">
          {estimate.notes.map((n, i) => (
            <li key={i} className={`text-xs ${estimate.fits ? "text-gray-500" : "text-accent-yellow"}`}>{n}</li>
          ))}
        </ul>
      )}
    </div>
  );
}
