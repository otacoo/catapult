export default function CatapultIcon({
  size = 24,
  className = "",
}: {
  size?: number;
  className?: string;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <line x1="3" y1="21" x2="21" y2="21" />
      <circle cx="6" cy="21" r="1.5" />
      <circle cx="18" cy="21" r="1.5" />
      <line x1="9" y1="21" x2="12" y2="9" />
      <line x1="15" y1="21" x2="12" y2="9" />
      <line x1="10.5" y1="15" x2="13.5" y2="15" />
      <line x1="4" y1="4" x2="18" y2="13" />
      <rect x="16.5" y="12.5" width="3" height="2.5" rx="0.5" fill="currentColor" stroke="none" />
      <circle cx="3.5" cy="3" r="2" fill="currentColor" stroke="none" />
    </svg>
  );
}
