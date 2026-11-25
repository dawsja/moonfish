import { hideGoalMarkers } from "@/lib/goals"
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react"
import { ArrowDown, Check, ChevronRight, CircleAlert, Copy, Info, ListChecks } from "lucide-react"
import { blocks, formatDuration, formatTokens, type Decision, type Item, type Transcript as T } from "@/lib/chat"
import { copyText } from "@/lib/clipboard"
import { cn } from "@/lib/utils"
import { Tip } from "@/components/Tip"
import { Markdown } from "./Markdown"
import { MessageImages } from "./ChatImages"
import { WorkGroup } from "./Work"
import { useTouch } from "./touch"

/** Blocks mounted at once; older ones come in by this many. */
const WINDOW = 150
/** The edges fade in over this much scrolling past them: under the header, and at the end,
 *  where the input floats on the transcript (`--dock`, its height, set by the chat around it). */
const FADE_TOP = 32
const FADE_END = 56
/** The end's fade starts this far above the input and runs on under it, out at the pane's edge. */
const FADE_ABOVE = 24
/** The jump-to-latest arrow shows this far from the end, not at the first nudge up. */
const JUMP_AFTER = 360

/** For an input that floats on the transcript's end: refs for the box holding both (`stage`) and
 *  the input's (`tray`), whose height goes on the stage as `--dock`, the room the transcript
 *  leaves at its end. Callback refs, so a stage or tray that comes back as a new element (the
 *  chat returning from a subagent's thread) is measured again; without `--dock` the end of the
 *  transcript stays under the input, out of scrolling's reach. */
export function useDock() {
  const [stage, setStage] = useState<HTMLDivElement | null>(null)
  const [tray, setTray] = useState<HTMLDivElement | null>(null)
  useLayoutEffect(() => {
    if (!stage || !tray) return
    const size = () => stage.style.setProperty("--dock", `${tray.offsetHeight}px`)
    size()
    const watch = new ResizeObserver(size)
    watch.observe(tray)
    return () => watch.disconnect()
  }, [stage, tray])
  return { stage: setStage, tray: setTray }
}

export type JumpTarget = { id: string; seq: number }

/** The conversation: user prompts on the right, the agent's work and answers on the left. */
export function Transcript({
  transcript,
  jumpTarget,
  className,
}: {
  transcript: T
  jumpTarget?: JumpTarget | null
  className?: string
}) {
  const scroller = useRef<HTMLDivElement>(null)
  const frame = useRef<HTMLDivElement>(null)
  const touch = useTouch()
  const [stuck, setStuck] = useState(true)
  const [far, setFar] = useState(false)
  const [jumpingToEnd, setJumpingToEnd] = useState(false)
  const all = useMemo(() => blocks(transcript.items), [transcript.items])
  // Long chats mount only their latest blocks; older ones load on request.
  const [limit, setLimit] = useState(WINDOW)
  const hidden = Math.max(0, all.length - limit)
  const list = hidden ? all.slice(hidden) : all
  const lastWork = list.findLastIndex((b) => b.kind === "work")

  const stuckRef = useRef(stuck)
  const touching = useRef(false)
  const jumping = useRef(false)
  const jumpHeight = useRef(0)
  const jumpSettle = useRef<number | null>(null)
  const handledJumpSeq = useRef(0)
  const stopJump = useCallback(() => {
    if (jumpSettle.current !== null) {
      clearTimeout(jumpSettle.current)
      jumpSettle.current = null
    }
    if (!jumping.current) return
    jumping.current = false
    setJumpingToEnd(false)
  }, [])
  const follow = useCallback((value: boolean) => {
    stuckRef.current = value
    setStuck(value)
  }, [])
  const jumpToLatest = () => {
    const el = scroller.current
    if (!el) return
    const rest = el.scrollHeight - el.scrollTop - el.clientHeight
    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches
    if (rest <= 2 || reduced) {
      stopJump()
      el.scrollTop = el.scrollHeight
      follow(true)
      return
    }
    jumping.current = true
    jumpHeight.current = el.scrollHeight
    setJumpingToEnd(true)
    el.scrollTo({ top: el.scrollHeight, behavior: "smooth" })
  }
  const lastUser = transcript.items.findLast((i) => i.kind === "user")?.id
  const seenUser = useRef(lastUser)
  useLayoutEffect(() => {
    const el = scroller.current
    if (lastUser !== seenUser.current) {
      seenUser.current = lastUser
      if (lastUser && !stuckRef.current) {
        stopJump()
        follow(true)
      }
    }
    if (el && stuckRef.current && !touching.current && !jumping.current) el.scrollTop = el.scrollHeight
  })
  useEffect(() => {
    const el = scroller.current
    if (!el) return
    let previousTop = el.scrollTop
    let touchY = 0
    // The fades show as much as there is past each edge, up to all of them: none at the very
    // top or end, so there the transcript stops cleanly under the header and over the input.
    const shade = () => {
      const box = frame.current
      if (!box) return
      const top = Math.max(0, el.scrollTop)
      const rest = Math.max(0, el.scrollHeight - top - el.clientHeight)
      box.style.setProperty("--fade-top", String(Math.min(1, top / FADE_TOP)))
      box.style.setProperty("--fade-end", String(Math.min(1, rest / FADE_END)))
      box.style.setProperty("--scrollbar", `${el.offsetWidth - el.clientWidth}px`)
      setFar(rest > JUMP_AFTER)
    }
    shade()
    const onScroll = () => {
      shade()
      const top = Math.max(0, el.scrollTop)
      const rest = el.scrollHeight - top - el.clientHeight
      const atBottom = rest <= 2
      if (jumping.current) {
        if (jumpSettle.current !== null) clearTimeout(jumpSettle.current)
        if (rest <= 4) {
          stopJump()
          follow(true)
        } else if (top < previousTop) {
          stopJump()
          follow(false)
        } else {
          // Fallback for webviews without scrollend.
          jumpSettle.current = window.setTimeout(onScrollEnd, 180)
        }
        previousTop = top
        return
      }
      // A move up that still ends at the bottom isn't the reader: the content got shorter
      // (finished steps collapsing, estimated block heights settling) and the browser
      // clamped the scroll position, so keep following.
      if (top < previousTop && !atBottom) follow(false)
      else if (top > previousTop && atBottom) follow(true)
      previousTop = top
    }
    const onScrollEnd = () => {
      if (!jumping.current) return
      const top = Math.max(0, el.scrollTop)
      const rest = el.scrollHeight - top - el.clientHeight
      if (rest <= 2) {
        stopJump()
        follow(true)
      } else if (el.scrollHeight > jumpHeight.current + 2) {
        jumpHeight.current = el.scrollHeight
        el.scrollTo({ top: el.scrollHeight, behavior: "smooth" })
      } else {
        stopJump()
        if (rest <= 8) follow(true)
      }
    }
    const onTouchStart = (event: TouchEvent) => {
      touching.current = true
      touchY = event.touches[0].clientY
    }
    const onTouchMove = (event: TouchEvent) => {
      const y = event.touches[0].clientY
      if (y > touchY) {
        stopJump()
        follow(false)
      }
      touchY = y
    }
    const onTouchEnd = () => { touching.current = false }
    const onWheel = (event: WheelEvent) => {
      if (event.deltaY < 0) {
        stopJump()
        follow(false)
      }
    }
    el.addEventListener("touchstart", onTouchStart, { passive: true })
    el.addEventListener("touchmove", onTouchMove, { passive: true })
    el.addEventListener("touchend", onTouchEnd, { passive: true })
    el.addEventListener("touchcancel", onTouchEnd, { passive: true })
    el.addEventListener("wheel", onWheel, { passive: true })
    el.addEventListener("scroll", onScroll, { passive: true })
    el.addEventListener("scrollend", onScrollEnd, { passive: true })
    const pin = new ResizeObserver(() => {
      if (stuckRef.current && !touching.current) el.scrollTop = el.scrollHeight
      else if (jumping.current && !touching.current && el.scrollHeight > jumpHeight.current + 2) {
        jumpHeight.current = el.scrollHeight
        el.scrollTo({ top: el.scrollHeight, behavior: "smooth" })
      }
      shade()
    })
    pin.observe(el)
    if (el.firstElementChild) pin.observe(el.firstElementChild)
    return () => {
      if (jumpSettle.current !== null) clearTimeout(jumpSettle.current)
      el.removeEventListener("scroll", onScroll)
      el.removeEventListener("scrollend", onScrollEnd)
      el.removeEventListener("touchstart", onTouchStart)
      el.removeEventListener("touchmove", onTouchMove)
      el.removeEventListener("touchend", onTouchEnd)
      el.removeEventListener("touchcancel", onTouchEnd)
      el.removeEventListener("wheel", onWheel)
      pin.disconnect()
    }
  }, [follow, stopJump])

  useEffect(() => {
    if (!jumpTarget || handledJumpSeq.current === jumpTarget.seq) return
    const idx = all.findIndex((b) => (b.kind === "work" ? b.items.some((it) => it.id === jumpTarget.id) : b.item.id === jumpTarget.id))
    if (idx < 0) return
    stopJump()
    follow(false)
    const needed = all.length - idx
    if (needed > limit) {
      setLimit(needed)
      return
    }
    handledJumpSeq.current = jumpTarget.seq
    let frames = 0
    let cancelled = false
    const locate = () => {
      if (cancelled) return
      const el = Array.from(scroller.current?.querySelectorAll<HTMLElement>("[data-item-id]") ?? []).find((node) => node.dataset.itemId === jumpTarget.id)
      if (el) {
        el.scrollIntoView({ behavior: "smooth", block: "center" })
        el.animate(
          [
            { backgroundColor: "color-mix(in srgb, var(--primary) 22%, transparent)", boxShadow: "0 0 0 2px color-mix(in srgb, var(--primary) 55%, transparent)" },
            { backgroundColor: "transparent", boxShadow: "0 0 0 0 transparent" },
          ],
          { duration: 1500, easing: "ease-out" },
        )
        return
      }
      if (++frames < 4) requestAnimationFrame(locate)
    }
    requestAnimationFrame(locate)
    return () => {
      cancelled = true
    }
  }, [jumpTarget, all, limit, stopJump, follow])

  return (
    <div ref={frame} className={cn("relative min-h-0", className)}>
      <div ref={scroller} className="h-full overflow-x-hidden overflow-y-auto">
        <div
          className={cn(
            "flex flex-col gap-4",
            touch ? "mx-auto w-full max-w-[46rem] px-4 pt-4" : "chat-col px-6 pt-6",
          )}
          // `--lead`: room at the top, for a header floating over the transcript (the phone's).
          style={{ paddingTop: `calc(var(--lead, 0px) + ${touch ? "1rem" : "1.5rem"})`, paddingBottom: `calc(var(--dock, 0px) + ${touch ? "1.5rem" : "2.5rem"})` }}
        >
          {hidden ? (
            <button
              className="self-center rounded-full border px-3.5 py-1 text-[12px] text-muted-foreground hover:bg-accent hover:text-foreground"
              onClick={() => {
                const el = scroller.current
                const from = el ? el.scrollHeight - el.scrollTop : 0
                setLimit((n) => n + WINDOW)
                // Keep the reader where they were as older blocks appear above.
                requestAnimationFrame(() => el && (el.scrollTop = el.scrollHeight - from))
              }}
            >
              Show {Math.min(hidden, WINDOW)} earlier
            </button>
          ) : null}
          {list.map((b, i) => (
            // Keep real heights on phones: estimated heights changing during a
            // swipe can shift the viewport and interrupt momentum scrolling.
            <div key={b.kind === "work" ? b.id : `${b.item.kind}-${b.item.id}`} data-item-id={b.kind === "item" ? b.item.id : undefined} className={cn(!touch && "[contain-intrinsic-size:auto_120px] [content-visibility:auto]")}>
              {b.kind === "work" ? (
                <WorkGroup items={b.items} live={transcript.running && i === lastWork && i >= list.length - 2} jumpTarget={jumpTarget} />
              ) : (
                <Row item={b.item} />
              )}
            </div>
          ))}
          {transcript.running && !transcript.approvals.length && !transcript.questions.length && list[list.length - 1]?.kind !== "work" ? <span className="shimmer self-start text-[12.5px]">Working…</span> : null}
        </div>
      </div>
      {/* The scrollbar stays clear of them. The end's goes on behind the input, the transcript
          still dimly there under it and gone by the pane's edge. */}
      <div aria-hidden className="pointer-events-none absolute top-0 left-0 right-(--scrollbar)" style={{ height: FADE_TOP, opacity: "var(--fade-top, 0)", background: "linear-gradient(var(--background), transparent)" }} />
      <div
        aria-hidden
        className="pointer-events-none absolute bottom-0 left-0 right-(--scrollbar)"
        style={{
          height: `calc(var(--dock, 0px) + ${FADE_ABOVE}px)`,
          opacity: "var(--fade-end, 0)",
          background: "linear-gradient(transparent, color-mix(in srgb, var(--background) 55%, transparent) 70%, var(--background))",
        }}
      />
      <div
        style={{ bottom: "calc(var(--dock, 0px) + 0.75rem)" }}
        className="pointer-events-none absolute inset-x-0"
      >
        <div className={cn("flex justify-center", touch ? "mx-auto w-full max-w-[46rem] px-4" : "chat-col px-6")}>
          <button
            type="button"
            aria-hidden={stuck || !far || jumpingToEnd}
            tabIndex={!stuck && far && !jumpingToEnd ? 0 : -1}
            className={cn(
              "flex items-center gap-1.5 rounded-full border bg-popover font-medium whitespace-nowrap text-muted-foreground shadow-lg transition-[opacity,transform,color] duration-200 ease-[cubic-bezier(0.22,1,0.36,1)] hover:text-foreground",
              touch ? "h-10 px-4 text-[13px]" : "h-8 px-3 text-[12px]",
              !stuck && far && !jumpingToEnd ? "pointer-events-auto translate-y-0 opacity-100" : "pointer-events-none translate-y-2 opacity-0",
            )}
            onClick={jumpToLatest}
          >
            <ArrowDown className={touch ? "size-4" : "size-3.5"} />
            <span>Jump to latest</span>
          </button>
        </div>
      </div>
    </div>
  )
}

function Row({ item }: { item: Item }) {
  switch (item.kind) {
    case "user":
      return (
        <div className="flex flex-col items-end gap-1">
          <div className="rounded-2xl max-w-[85%] border bg-user px-3.5 py-2.5 text-[length:var(--moonfish-chat-size,14px)] leading-relaxed whitespace-pre-wrap select-text [overflow-wrap:anywhere]">
            {item.text}
            <MessageImages files={item.files} count={item.images.length} />
          </div>
        </div>
      )
    case "assistant":
      return <AssistantText text={hideGoalMarkers(item.text)} />
    case "question": {
      // What the agent asked and what it was told (live questions are the card above the box).
      if (!item.answers) return null
      const answers = item.answers
      return (
        <div className="flex flex-col gap-1 border-l-2 border-input pl-3 text-[12.5px] text-muted-foreground">
          {item.request.questions.map((q) => {
            const a = answers[q.id] ?? []
            return (
              <div key={q.id} className="select-text [overflow-wrap:anywhere]">
                <span className="text-foreground/80">{q.question}</span>. Answer: {a.length ? a.join(", ") : <span className="text-faint">skipped</span>}
              </div>
            )
          })}
        </div>
      )
    }
    case "approval":
      if (item.decision && item.request.kind === "plan" && item.request.detail) return <PlanCard text={item.request.detail} decision={item.decision} />
      return item.decision ? (
        <div className="flex items-center gap-2 text-[12px] text-faint">
          <span className={cn("size-1.5", item.decision === "deny" ? "bg-red" : "bg-green")} />
          {item.decision === "deny" ? "Denied" : item.decision === "allowAlways" ? "Allowed for this chat" : "Allowed"}: {item.request.title}
        </div>
      ) : null
    case "turnEnd":
      return (
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 font-mono text-[11px] text-faint">
          {item.interrupted ? <span className="text-warn">Interrupted</span> : null}
          {item.durationMs ? <span>{formatDuration(item.durationMs)}</span> : null}
          {item.usage ? (
            <span title={`${item.usage.inputTokens} in · ${item.usage.outputTokens} out · ${item.usage.cacheWriteTokens} written to cache · ${item.usage.cacheReadTokens} read from cache`}>
              · {formatTokens(item.usage.inputTokens + item.usage.outputTokens + item.usage.cacheWriteTokens)} tokens
              {item.usage.cacheReadTokens ? ` · ${formatTokens(item.usage.cacheReadTokens)} cached` : null}
            </span>
          ) : null}
          {item.costUsd ? <span>· ${item.costUsd.toFixed(2)}</span> : null}
        </div>
      )
    case "notice":
      return (
        <div className={cn("rounded-lg flex items-start gap-2 border px-3 py-2 text-[12.5px]", item.level === "error" ? "border-red/40 bg-del/40 text-red" : item.level === "warn" ? "border-warn/40 text-warn" : "text-muted-foreground")}>
          {item.level === "info" ? <Info className="mt-0.5 size-3.5 shrink-0" /> : <CircleAlert className="mt-0.5 size-3.5 shrink-0" />}
          <span className="select-text">{item.text}</span>
        </div>
      )
    default:
      return null
  }
}

/** A plan the agent asked to start on, kept in the chat once answered. */
function PlanCard({ text, decision }: { text: string; decision: Decision }) {
  const [open, setOpen] = useState(decision !== "deny")
  return (
    <div className="overflow-hidden rounded-lg border bg-card">
      <button className="flex min-h-10 w-full items-center gap-2 px-3 py-2 text-left text-[12.5px]" onClick={() => setOpen(!open)}>
        <ListChecks className="size-3.5 text-muted-foreground" />
        <span className="font-medium">Plan</span>
        <span className="text-faint">{decision === "deny" ? "· kept planning" : "· approved"}</span>
        <ChevronRight className={cn("ml-auto size-3.5 text-faint transition-transform", open && "rotate-90")} />
      </button>
      {open ? <Markdown text={text} className="border-t px-3.5 py-2.5" /> : null}
    </div>
  )
}

function AssistantText({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  const touch = useTouch()
  return (
    <div className="group/msg relative flex flex-col items-start">
      <div className="w-full">
        <Markdown text={text} />
      </div>
      <Tip content={copied ? "Copied" : "Copy"}>
        <button
          className={cn(
            "grid place-items-center text-faint hover:bg-accent hover:text-foreground",
            touch ? "-ml-1.5 size-10 rounded-full" : "-ml-1 mt-0.5 size-6 rounded-md opacity-0 group-hover/msg:opacity-100 focus-visible:opacity-100",
            copied && "text-foreground opacity-100",
          )}
          aria-label={copied ? "Copied" : "Copy message"}
          onClick={() => {
            copyText(text)
            setCopied(true)
            setTimeout(() => setCopied(false), 1200)
          }}
        >
          {copied ? <Check className={touch ? "size-4" : "size-3.5"} /> : <Copy className={touch ? "size-4" : "size-3.5"} />}
        </button>
      </Tip>
    </div>
  )
}
