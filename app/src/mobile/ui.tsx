// Small pieces the phone screens share: the glass capsule each screen floats at its top, round
// buttons, the link status, and how long ago.

import type { ReactNode } from "react"
import { ChevronLeft, Loader2 } from "lucide-react"
import { cn } from "@/lib/utils"
import { native } from "./rpc"
import { back } from "./route"
import { useMobile } from "./store"

/** Frosted glass: the capsule, and what floats over the content with it. */
export const GLASS = "border bg-card/90 backdrop-blur-xl shadow-[0_10px_30px_-14px_rgb(0_0_0/0.45)]"
/** The room a screen leaves at its top for the capsule (and the status bar above it). */
export const UNDER_CAPSULE = "pt-[calc(var(--safe-top)+68px)]"

/** A screen's header: a glass capsule floating over its top. The content scrolls on behind it
 *  and fades out at the top of the screen, under the status bar (solid at the very top, so
 *  the phone's clock and icons stay readable, and clear by the capsule's top edge). */
export function Capsule({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <>
      <div
        aria-hidden
        className="pointer-events-none absolute inset-x-0 top-0 z-10 h-[calc(var(--safe-top)+16px)]"
        style={{ background: "linear-gradient(to bottom, var(--background) calc(var(--safe-top) * 0.6), transparent)" }}
      />
      <header className={cn("absolute inset-x-3 top-[calc(var(--safe-top)+8px)] z-20 flex h-[52px] items-center gap-1 rounded-full p-1.5", GLASS, className)}>{children}</header>
    </>
  )
}

/** A capsule with back on the left (none with `onBack` null), a title (and what it's about) in
 *  the middle, and actions. */
export function TitleCapsule({ title, subtitle, actions, onBack = back }: { title: ReactNode; subtitle?: ReactNode; actions?: ReactNode; onBack?: (() => void) | null }) {
  return (
    <Capsule>
      {onBack ? (
        <RoundButton label="Back" onClick={onBack} className="bg-background">
          <ChevronLeft className="size-5" />
        </RoundButton>
      ) : null}
      <div className={cn("min-w-0 flex-1", onBack ? "px-1.5" : "px-3")}>
        <div className="truncate text-[14.5px] leading-tight font-semibold">{title}</div>
        {subtitle ? <div className="truncate text-[11.5px] text-muted-foreground">{subtitle}</div> : null}
      </div>
      {actions}
    </Capsule>
  )
}

/** A round icon button sized for a thumb. */
export function RoundButton({ label, onClick, children, className, disabled }: { label: string; onClick?: () => void; children: ReactNode; className?: string; disabled?: boolean }) {
  return (
    <button className={cn("grid size-10 shrink-0 place-items-center rounded-full text-muted-foreground active:bg-accent disabled:opacity-40", className)} aria-label={label} onClick={onClick} disabled={disabled}>
      {children}
    </button>
  )
}

/** The link to the desktop, when it isn't up: connecting, offline (with why), or removed. */
export function LinkBanner({ className }: { className?: string }) {
  const link = useMobile((s) => s.link)
  const error = useMobile((s) => s.linkError)
  const name = useMobile((s) => s.desktopName)
  if (link === "connected" || link === "idle") return null
  const text =
    link === "connecting"
      ? `Connecting to ${name || "the desktop"}…`
      : link === "revoked"
        ? "Pair this phone again. The computer removed it."
        : `Can't reach ${name || "the desktop"}${error ? `: ${error}` : ""}`
  return (
    <div
      className={cn("flex items-center gap-2.5 rounded-[20px] border px-3.5 py-1.5 text-[12.5px]", link === "connecting" ? "bg-card text-muted-foreground" : "border-red/30 bg-card text-red", className)}
      role="status"
    >
      {link === "connecting" ? <Loader2 className="size-3.5 shrink-0 animate-spin" /> : null}
      <span className="line-clamp-3 min-w-0 flex-1 py-1 leading-snug">{text}</span>
      {link === "offline" ? (
        <button className="h-9 shrink-0 rounded-full border border-red/40 px-3.5 font-medium active:bg-red/10" onClick={() => void native.wake()}>
          Retry
        </button>
      ) : null}
    </div>
  )
}

/** "now", "5m", "3h", "2d", or the date. */
export function ago(ms: number, now = Date.now()): string {
  const s = Math.max(0, (now - ms) / 1000)
  if (s < 60) return "now"
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`
  if (s < 7 * 86400) return `${Math.floor(s / 86400)}d`
  return new Date(ms).toLocaleDateString()
}
