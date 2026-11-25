import { cn } from "@/lib/utils"

export function Spinner({ className, label }: { className?: string; label?: string }) {
  return (
    <svg viewBox="0 0 24 24" className={cn("moonfish-spinner size-3.5 shrink-0", className)} role={label ? "img" : undefined} aria-label={label} aria-hidden={label ? undefined : true}>
      <circle cx="12" cy="12" r="8.6" fill="none" stroke="currentColor" strokeOpacity="0.2" strokeWidth="2.4" />
      <path d="M12 3.4A8.6 8.6 0 0 1 20.6 12" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" />
    </svg>
  )
}
