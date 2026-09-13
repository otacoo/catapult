export default function Toggle({ label, hint, flag, checked, onChange }: {
  label?: string; hint?: string; flag?: string; checked: boolean; onChange: (v: boolean) => void;
}) {
  return (
    <div className="flex items-start gap-3">
      <button role="switch" aria-checked={checked} onClick={() => onChange(!checked)}
        className={`relative shrink-0 w-8 h-4 rounded-full transition-colors mt-0.5 ${checked ? "bg-primary" : "bg-surface-4"}`}>
        <span className={`absolute top-0.5 left-0.5 w-3 h-3 rounded-full bg-white shadow transition-transform ${checked ? "translate-x-4" : ""}`} />
      </button>
      {(label || hint) && (
        <div>
          {label && <p className="text-xs font-medium text-gray-300">{label}{flag && <code className="ml-1.5 rounded bg-surface-3 px-1 py-px font-mono text-[10px] font-normal text-gray-500">{flag}</code>}</p>}
          {hint && <p className="text-xs text-gray-600">{hint}</p>}
        </div>
      )}
    </div>
  );
}
