// The phone's sheets: they pull in over the screen from the bottom (or drop from the top),
// with a handle to drag them back out, and Android's back button closes them.

import { useEffect, useId, useRef, type CSSProperties, type PointerEvent, type ReactNode } from "react"
import { Dialog } from "radix-ui"
import { cn } from "@/lib/utils"

/** Sheets open now, by their history entry: a back press closes the top one. */
const open = new Set<string>()

/** While `shown`, the sheet has a history entry of its own, so Android's back button (the
 *  WebView's history) closes it instead of leaving the screen. Closed another way, the entry
 *  goes again. */
export function useBackToClose(shown: boolean, close: () => void, enabled = true) {
  const id = useId()
  const latest = useRef(close)
  latest.current = close
  useEffect(() => {
    if (!shown || !enabled) return
    // StrictMode runs this twice: the second run finds its entry still on top and keeps it.
    if (history.state?.sheet !== id) history.pushState({ sheet: id }, "")
    open.add(id)
    const onPop = () => {
      if (history.state?.sheet === id) return
      open.delete(id)
      latest.current()
    }
    window.addEventListener("popstate", onPop)
    return () => {
      window.removeEventListener("popstate", onPop)
      open.delete(id)
      setTimeout(() => {
        if (!open.has(id) && history.state?.sheet === id) history.back()
      }, 0)
    }
  }, [shown, enabled, id])
}

/** A sheet's handle. */
export function Grabber({ className }: { className?: string }) {
  return <div aria-hidden className={cn("mx-auto h-1 w-10 shrink-0 rounded-full bg-foreground/20", className)} />
}

/** Dragging a sheet by its handle: it follows the finger toward its edge (and a little the
 *  other way), and a pull far enough either way calls `onPull`. */
function useDrag(side: "top" | "bottom", onPull: (dir: "out" | "in") => void) {
  const start = useRef<{ y: number; id: number } | null>(null)
  const sheet = useRef<HTMLDivElement>(null)
  const set = (dy: number, animate: boolean) => {
    const el = sheet.current
    if (!el) return
    el.style.transition = animate ? "transform 220ms cubic-bezier(.2,.8,.2,1)" : "none"
    el.style.transform = dy ? `translateY(${dy}px)` : ""
  }
  const out = side === "bottom" ? 1 : -1
  return {
    sheet,
    handle: {
      onPointerDown: (e: PointerEvent) => {
        if ((e.target as HTMLElement).closest("button, input, textarea, a")) return
        start.current = { y: e.clientY, id: e.pointerId }
        e.currentTarget.setPointerCapture(e.pointerId)
      },
      onPointerMove: (e: PointerEvent) => {
        if (!start.current) return
        const dy = e.clientY - start.current.y
        // Toward the edge it follows; away from it, it resists.
        set(dy * out > 0 ? dy : dy / 4, false)
      },
      onPointerUp: (e: PointerEvent) => {
        if (!start.current) return
        const dy = (e.clientY - start.current.y) * out
        start.current = null
        set(0, true)
        if (dy > 70) onPull("out")
        else if (dy < -50) onPull("in")
      },
      onPointerCancel: () => {
        start.current = null
        set(0, true)
      },
    },
  }
}

type Props = {
  open: boolean
  onClose: () => void
  /** Bottom (the default) or dropped from the top. */
  side?: "bottom" | "top"
  /** For screen readers; shown when `header` isn't given. */
  title: string
  description?: string
  /** What sits in the handle's strip (drag it to pull the sheet). */
  header?: ReactNode
  children: ReactNode
  className?: string
  /** A pull past the handle: return true if handled (another height), else out closes it. */
  onPull?: (dir: "out" | "in") => boolean
  /** Its own history entry (Android back closes it); off when a route opened it. */
  history?: boolean
  /** A lighter dim behind, for a sheet you read the screen around. */
  light?: boolean
  style?: CSSProperties
}

export function PullSheet({ open: shown, onClose, side = "bottom", title, description, header, children, className, onPull, history: own = true, light, style }: Props) {
  useBackToClose(shown, onClose, own)
  const { sheet, handle } = useDrag(side, (dir) => {
    if (onPull?.(dir)) return
    if (dir === "out") onClose()
  })
  const grab = <Grabber className={side === "bottom" ? "mt-2.5 mb-1.5" : "mt-1.5 mb-2.5"} />
  return (
    <Dialog.Root open={shown} onOpenChange={(o) => !o && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay
          className={cn(
            "fixed inset-0 z-50 data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:animate-in data-[state=open]:fade-in-0",
            // Light: the screen above stays sharp, to read while the sheet is up.
            light ? "bg-black/25" : "bg-black/45 backdrop-blur-[2px]",
          )}
        />
        <Dialog.Content
          ref={sheet}
          onOpenAutoFocus={(e) => e.preventDefault()}
          style={style}
          className={cn(
            "fixed z-50 flex flex-col border bg-popover text-popover-foreground shadow-[0_24px_60px_-12px_rgb(0_0_0/0.6)] outline-none data-[state=closed]:animate-out data-[state=open]:animate-in data-[state=closed]:duration-200 data-[state=open]:duration-300",
            side === "bottom"
              ? "inset-x-0 bottom-0 max-h-[92dvh] rounded-t-[34px] border-x-0 border-b-0 pb-[max(0.75rem,var(--safe-bottom))] data-[state=closed]:slide-out-to-bottom data-[state=open]:slide-in-from-bottom"
              : "inset-x-2 top-[calc(var(--safe-top)+8px)] max-h-[85dvh] rounded-[34px] data-[state=closed]:slide-out-to-top data-[state=open]:slide-in-from-top",
            className,
          )}
        >
          <Dialog.Title className="sr-only">{title}</Dialog.Title>
          <Dialog.Description className="sr-only">{description ?? title}</Dialog.Description>
          {side === "bottom" ? (
            <div className="shrink-0 touch-none" {...handle}>
              {grab}
              {header}
            </div>
          ) : null}
          {children}
          {side === "top" ? (
            <div className="shrink-0 touch-none" {...handle}>
              {header}
              {grab}
            </div>
          ) : null}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  )
}

/** A sheet's heading row: the title, and what it's about under it. */
export function SheetHead({ title, sub, aside }: { title: ReactNode; sub?: ReactNode; aside?: ReactNode }) {
  return (
    <div className="flex items-center gap-2 px-4 pt-1 pb-3">
      <div className="min-w-0 flex-1">
        <h2 className="truncate text-[18px] leading-tight font-semibold">{title}</h2>
        {sub ? <div className="truncate text-[12.5px] text-muted-foreground">{sub}</div> : null}
      </div>
      {aside}
    </div>
  )
}
