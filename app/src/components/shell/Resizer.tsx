import { useRef } from "react"

export function Resizer({
  width,
  defaultWidth,
  onWidth,
  onDragChange,
  side,
  min,
  max,
  disabled = false,
}: {
  width: number
  defaultWidth: number
  onWidth: (w: number) => void
  onDragChange?: (dragging: boolean) => void
  side: "left" | "right"
  min: number
  max: number
  disabled?: boolean
}) {
  const start = useRef<{ at: number; w: number } | null>(null)
  if (disabled) return <div aria-hidden className="w-2 shrink-0" />
  const stop = () => {
    if (!start.current) return
    start.current = null
    onDragChange?.(false)
  }
  return (
    <div
      role="separator"
      title="Drag to resize · Double-click to reset"
      onDoubleClick={() => {
        stop()
        onWidth(Math.min(max, Math.max(min, defaultWidth)))
      }}
      aria-orientation="vertical"
      className="group/rs relative z-10 w-2 shrink-0 cursor-col-resize after:absolute after:inset-y-[40%] after:left-1/2 after:w-[3px] after:-translate-x-1/2 after:rounded-full after:transition-colors hover:after:bg-ring/60 active:after:bg-ring"
      onPointerDown={(e) => {
        start.current = { at: e.clientX, w: width }
        onDragChange?.(true)
        e.currentTarget.setPointerCapture(e.pointerId)
      }}
      onPointerMove={(e) => {
        if (!start.current) return
        const d = e.clientX - start.current.at
        const w = start.current.w + (side === "left" ? d : -d)
        onWidth(Math.round(Math.min(max, Math.max(min, w))))
      }}
      onPointerUp={stop}
      onPointerCancel={stop}
      onLostPointerCapture={stop}
    />
  )
}
