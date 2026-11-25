import { useEffect, useMemo, useRef, useState } from "react"
import { ArrowLeft, ArrowUp, Check, CircleSlash, LoaderCircle, Square, X } from "lucide-react"
import { toast } from "sonner"
import { Tip } from "@/components/Tip"
import { emptyTranscript, reduce, replay, type ChatEvent, type Transcript as T } from "@/lib/chat"
import { ipc, onSubagentEvent } from "@/lib/ipc"
import type { SubagentItem } from "@/lib/subagents"
import { cn } from "@/lib/utils"
import { Transcript, useDock } from "./Transcript"

export function SubagentView({
  chatId,
  subagent,
  onBack,
  onStop,
}: {
  chatId: string
  subagent: SubagentItem
  onBack: () => void
  onStop?: () => void
}) {
  const taskId = subagent.taskId ?? subagent.id
  const [transcript, setTranscript] = useState<T>(() => emptyTranscript())
  const [text, setText] = useState("")
  const [sending, setSending] = useState(false)
  const boxRef = useRef<HTMLTextAreaElement>(null)
  const { stage, tray } = useDock()

  useEffect(() => {
    let active = true
    let loaded = false
    let snapSeq = 0
    const held: [number, ChatEvent][] = []
    let unlisten: (() => void) | undefined

    setTranscript(emptyTranscript())

    const setup = async () => {
      unlisten = await onSubagentEvent((cid, tid, event, seq) => {
        if (!active || cid !== chatId || tid !== taskId) return
        if (!loaded) {
          held.push([seq, event])
          return
        }
        if (seq <= snapSeq) return
        setTranscript((cur) => reduce(cur, event))
      })
      if (!active) {
        unlisten?.()
        return
      }
      const snap = await ipc.chatSubagentSnapshot(chatId, taskId).catch(() => ({ events: [] as ChatEvent[], seq: 0, busy: false }))
      if (!active) return
      snapSeq = snap.seq
      loaded = true
      const base = replay(snap.events, snap.busy, true)
      const merged = held.filter(([seq]) => seq > snap.seq).reduce((t, [, ev]) => reduce(t, ev), base)
      setTranscript(merged)
    }

    void setup()
    return () => {
      active = false
      unlisten?.()
    }
  }, [chatId, taskId])

  useEffect(() => {
    const el = boxRef.current
    if (!el) return
    el.style.height = "auto"
    el.style.height = `${el.scrollHeight}px`
  }, [text])

  const live = subagent.status === "running" || transcript.running
  const displayTranscript = useMemo<T>(() => {
    const hasItems = transcript.items.length > 0
    if (hasItems) {
      return transcript.running === live ? transcript : { ...transcript, running: live }
    }
    if (subagent.prompt) {
      return {
        ...transcript,
        running: live,
        items: [{ kind: "user", id: `${taskId}:prompt`, text: subagent.prompt, images: [], files: [] }],
      }
    }
    return { ...transcript, running: live }
  }, [transcript, live, subagent.prompt, taskId])

  const send = async () => {
    const trimmed = text.trim()
    if (!trimmed || sending || !subagent.interactive) return
    setText("")
    setSending(true)
    try {
      await ipc.chatSubagentSend(chatId, taskId, { id: crypto.randomUUID(), text: trimmed, images: [] })
    } catch (e) {
      toast.error(String(e))
      setText((cur) => (cur.trim() ? cur : trimmed))
    } finally {
      setSending(false)
    }
  }

  const interrupt = async () => {
    if (subagent.stoppable && onStop) {
      onStop()
      return
    }
    try {
      await ipc.chatSubagentInterrupt(chatId, taskId)
    } catch (e) {
      toast.error(String(e))
    }
  }

  const showRole = !!subagent.role && subagent.role.toLowerCase() !== subagent.title.toLowerCase()
  const statusLabel = live ? "Running" : subagent.status === "completed" ? "Done" : subagent.status === "failed" ? "Failed" : "Stopped"

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col overflow-clip bg-background">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b px-3">
        <button
          type="button"
          className="rounded-md flex h-7 shrink-0 items-center gap-1.5 px-2 text-[12px] text-muted-foreground hover:bg-accent hover:text-foreground"
          onClick={onBack}
        >
          <ArrowLeft className="size-3.5" /> Back to chat
        </button>
        <span className="h-4 w-px shrink-0 bg-border" />
        {live ? (
          <span className="size-2 shrink-0 rounded-full bg-green animate-pulse" aria-label="Running" />
        ) : subagent.status === "completed" ? (
          <Check className="size-3.5 shrink-0 text-green" aria-label="Done" />
        ) : subagent.status === "failed" ? (
          <X className="size-3.5 shrink-0 text-red" aria-label="Failed" />
        ) : (
          <CircleSlash className="size-3.5 shrink-0 text-faint" aria-label="Stopped" />
        )}
        <span className="min-w-0 truncate text-[13px] font-medium">{subagent.title}</span>
        {showRole ? (
          <span className="max-w-36 shrink-0 truncate rounded-full border bg-muted/60 px-2 py-px font-mono text-[10.5px] text-muted-foreground">
            {subagent.role}
          </span>
        ) : null}
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <span className={cn("font-mono text-[11px]", live ? "text-green" : subagent.status === "failed" ? "text-red" : "text-faint")}>
            {statusLabel}
          </span>
          {live && (subagent.stoppable || subagent.interactive) ? (
            <button
              type="button"
              className="rounded-md flex h-7 items-center gap-1 border border-red/30 bg-del/50 px-2 text-[11.5px] font-medium text-red hover:bg-del"
              onClick={() => void interrupt()}
            >
              <X className="size-3" />
              <span>Stop</span>
            </button>
          ) : null}
        </div>
      </div>

      <div ref={stage} className="relative min-h-0 flex-1">
        <Transcript transcript={displayTranscript} className="h-full" />
        <div ref={tray} className="pointer-events-none absolute inset-x-0 bottom-0 flex max-h-[50%] min-h-0 flex-col">
          <div className="chat-col pointer-events-auto flex flex-col px-6 pb-4">
            {subagent.interactive ? (
              <div className="rounded-2xl border border-border bg-card shadow-[0_6px_20px_-12px_rgb(15_23_42/0.25)] dark:shadow-[0_1px_0_rgb(255_255_255/0.03)_inset,0_8px_24px_-12px_rgb(0_0_0/0.6)] focus-within:border-ring/70">
                <textarea
                  ref={boxRef}
                  aria-label={`Message ${subagent.title}`}
                  value={text}
                  rows={2}
                  placeholder={`Message ${subagent.title}…`}
                  className="block max-h-48 min-h-[56px] w-full resize-none bg-transparent px-4 pt-3 text-[length:var(--moonfish-chat-size,14px)] leading-relaxed outline-none placeholder:text-faint"
                  onChange={(e) => setText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                      e.preventDefault()
                      void send()
                    }
                  }}
                />
                <div className="flex items-center gap-1.5 px-3 pb-2.5">
                  <span className="truncate text-[11.5px] text-faint">
                    {sending ? "Sending…" : live ? "Subagent is running — you can message it directly" : "Send a follow-up to this subagent"}
                  </span>
                  <div className="ml-auto flex shrink-0 items-center gap-1">
                    {live ? (
                      <Tip content="Stop subagent">
                        <button
                          type="button"
                          className="rounded-full grid size-7 place-items-center bg-foreground text-background hover:opacity-90"
                          aria-label="Stop subagent"
                          onClick={() => void interrupt()}
                        >
                          <Square className="size-3 fill-current" />
                        </button>
                      </Tip>
                    ) : null}
                    <Tip content="Send to subagent">
                      <button
                        type="button"
                        className="rounded-full grid size-7 place-items-center bg-primary text-primary-foreground hover:bg-primary/90 disabled:bg-muted disabled:text-faint"
                        aria-label="Send to subagent"
                        disabled={!text.trim() || sending}
                        onClick={() => void send()}
                      >
                        {sending ? <LoaderCircle className="size-3.5 animate-spin" /> : <ArrowUp className="size-4" />}
                      </button>
                    </Tip>
                  </div>
                </div>
              </div>
            ) : (
              <div className="flex items-center justify-center rounded-xl border border-border/80 bg-card/90 px-4 py-2.5 text-[12px] text-muted-foreground shadow-sm backdrop-blur-sm">
                Watching live thread · direct replies aren&apos;t supported for this agent&apos;s subagents
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
