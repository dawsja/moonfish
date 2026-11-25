import { useEffect, useRef, useState, type ReactNode } from "react"
import { ArrowUp, Check, ChevronDown, Cpu, ImagePlus, ListPlus, Plus, Slash, Square, X, Zap } from "lucide-react"
import { Switch } from "@/components/ui/switch"
import { PERMISSION_ICON, PERMISSION_TONE } from "@/components/permission"
import { PullSheet, SheetHead } from "./sheet"
import { effortLabel, formatTokens, type ModelOption, type SlashCommand, type Transcript } from "@/lib/chat"
import { filterCommands, insertSkill, menuCommands, ownCommand, skillCommands, skillQuery, slashQuery, type MenuCommand, type OwnCommand } from "@/lib/commands"
import { readGoalCommand, type GoalInput } from "@/lib/goals"
import { PERMISSION_LABEL, type AgentKind, type PermissionMode } from "@/lib/types"
import { cn } from "@/lib/utils"
import { fitImage } from "./photos"
import { type Image } from "./rpc"
import { attachmentMime, MAX_IMAGE, readAttachment } from "@/lib/attachments"
import { openImages } from "@/components/ImageView"
import { toast } from "sonner"

const MODES: PermissionMode[] = ["ask", "acceptEdits", "plan", "fullAccess"]

type Props = {
  agent?: AgentKind
  running: boolean
  /** On a sheet: the box sinks into it instead of floating. */
  sunk?: boolean
  disabled?: boolean
  placeholder?: string
  goalHeader?: ReactNode
  goalActive?: boolean
  /** `now`: into the running turn; otherwise it waits for the turn to end. */
  /** Resolves false when the message didn't go: the box gets it back. */
  onSend: (text: string, images: Image[], how: "queue" | "now", permissionOverride?: PermissionMode) => void | Promise<boolean | void>
  onStop?: () => void
  models: ModelOption[]
  /** The model list couldn't be had (the desktop or its agent didn't answer). */
  modelsFailed?: boolean
  modelsError?: string | null
  model: string
  effort: string
  fastMode?: boolean
  onModel?: (model: string, effort: string, fastMode?: boolean) => void
  onModelsOpen?: () => void
  commands?: SlashCommand[]
  permission: PermissionMode | null
  onPermission?: (m: PermissionMode) => void | Promise<void>
  /** How full the agent's context window is, when it has said. */
  context?: Transcript["context"]
  onCompact?: (instructions: string) => void
  onClear?: () => void
  onGoal?: (input: GoalInput) => Promise<boolean>
  /** Over a chat that scrolls on behind it: only the box shows, no bar of its own. */
  floating?: boolean
}

/** The agent's default effort, as a choice beside the named ones. */
const DEFAULT_EFFORT = "default"

function Heading({ children, first }: { children: ReactNode; first?: boolean }) {
  return <h3 className={cn("mb-2 px-1 text-[12.5px] font-medium text-muted-foreground", first ? "mt-1" : "mt-4")}>{children}</h3>
}


export function MobileComposer(p: Props) {
  const [text, setText] = useState("")
  const [images, setImages] = useState<Image[]>([])
  const [sheet, setSheet] = useState<"more" | "model" | "commands" | null>(null)
  const [trigger, setTrigger] = useState<{ kind: "slash" | "skill"; start: number; query: string } | null>(null)
  const [clearArmed, setClearArmed] = useState(false)
  useEffect(() => {
    if (!clearArmed) return
    const t = setTimeout(() => setClearArmed(false), 3000)
    return () => clearTimeout(t)
  }, [clearArmed])
  const box = useRef<HTMLTextAreaElement>(null)
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => void (mounted.current = false)
  }, [])
  const file = useRef<HTMLInputElement>(null)

  // Grows with the text, up to about six lines.
  useEffect(() => {
    const el = box.current
    if (!el) return
    el.style.height = "auto"
    el.style.height = `${Math.min(el.scrollHeight, 150)}px`
  }, [text])

  const inChat = !!p.onClear
  const agentKind: AgentKind = p.agent ?? "claude"
  const allCommands = menuCommands(agentKind, p.commands ?? [], inChat)
  const pickerItems = trigger
    ? filterCommands(
        trigger.kind === "skill" ? skillCommands(p.commands ?? []) : allCommands,
        trigger.query,
      ).slice(0, 40)
    : []

  const updateTrigger = (value: string, caret: number) => {
    const sq = slashQuery(value, caret)
    if (sq !== null) {
      if (sq === "") p.onModelsOpen?.()
      setTrigger({ kind: "slash", start: 0, query: sq })
      return
    }
    const sk = skillQuery(value, caret)
    if (sk !== null) {
      if (sk.query === "") p.onModelsOpen?.()
      setTrigger({ kind: "skill", start: sk.start, query: sk.query })
      return
    }
    setTrigger(null)
  }

  const runOwn = async (own: OwnCommand, args: string, how: "queue" | "now", sent: Image[], rawText: string) => {
    if (own === "model") {
      if (!args) {
        p.onModelsOpen?.()
        setSheet("model")
        return
      }
      const a = args.toLowerCase()
      const m =
        p.models.find((x) => x.id.toLowerCase() === a || x.name.toLowerCase() === a) ??
        p.models.find((x) => x.id.toLowerCase().includes(a) || x.name.toLowerCase().includes(a))
      p.onModel?.(m?.id ?? args, p.effort)
      toast.success(`Model set to ${m?.name ?? args}`)
    } else if (own === "plan") {
      if (p.permission !== "plan" || !args) toast.success("The agent will propose a plan before making changes")
      await p.onPermission?.("plan")
      if (args) {
        const ok = await p.onSend(args, sent, how, "plan")
        if (ok === false && mounted.current) {
          setText((cur) => (cur.trim() ? cur : rawText))
          setImages((cur) => (cur.length ? cur : sent))
        }
      }
    } else if (own === "clear") {
      p.onClear?.()
    } else if (own === "compact") {
      p.onCompact?.(args)
    } else if (own === "goal") {
      const command = readGoalCommand(`/goal ${args}`)
      if (command && (await p.onGoal?.(command)) === false && mounted.current) {
        setText((current) => current.trim() || `/goal ${args}`.trim())
      }
    }
  }

  const pickCommand = (c: MenuCommand, active: typeof trigger) => {
    const el = box.current
    setTrigger(null)
    setSheet(null)
    if (active?.kind === "skill") {
      const caret = active.start + c.name.length + 2
      setText((t) => insertSkill(t, active, c.name).text)
      requestAnimationFrame(() => {
        el?.focus({ preventScroll: true })
        el?.setSelectionRange(caret, caret)
      })
      return
    }
    if (c.own) {
      setText("")
      void runOwn(c.own, "", "queue", [], `/${c.name}`)
      return
    }
    if (!c.skill && !c.hint) {
      setText("")
      void p.onSend(`/${c.name}`, [], "queue")
      return
    }
    const insert = c.skill && agentKind === "codex" ? `$${c.name} ` : `/${c.name} `
    setText(insert)
    requestAnimationFrame(() => {
      el?.focus({ preventScroll: true })
      el?.setSelectionRange(insert.length, insert.length)
    })
  }

  const addImages = async (picked: File[]) => {
    // A camera's photos are larger than the desktop takes: those are scaled down first.
    const files = await Promise.all(picked.slice(0, 8).map((f) => fitImage(f)))
    const valid = files.filter((f) => attachmentMime(f) && f.size <= MAX_IMAGE)
    if (files.some((f) => !attachmentMime(f))) toast.error("Attach a PNG, JPEG, GIF, or WebP image")
    if (files.some((f) => f.size > MAX_IMAGE)) toast.error("Images must be 5 MB or smaller")
    const results = await Promise.allSettled(valid.slice(0, 8).map(readAttachment))
    if (results.some((r) => r.status === "rejected")) toast.error("Could not read an image")
    const read = results.flatMap((r) => r.status === "fulfilled" ? [r.value] : [])
    setImages((a) => [...a, ...read].slice(0, 8))
  }
  const has = text.trim().length > 0 || images.length > 0
  const send = (how: "queue" | "now") => {
    if (!has || p.disabled) return
    const [t, sent] = [text.trim(), images]
    setTrigger(null)
    const own = ownCommand(t, inChat, agentKind)
    if (own && (!sent.length || (own.own === "plan" && !!own.args))) {
      const planImages = own.own === "plan" && own.args ? sent : []
      setText("")
      if (planImages.length) setImages([])
      void runOwn(own.own, own.args, how, planImages, t)
      return
    }
    void Promise.resolve(p.onSend(t || "(see the attached image)", sent, how)).then((ok) => {
      if (ok !== false || !mounted.current) return
      setText((cur) => (cur.trim() ? cur : t))
      setImages((cur) => (cur.length ? cur : sent))
    })
    setText("")
    setImages([])
  }
  const current = p.models.find((m) => m.id === p.model)

  return (
    <div onDragOver={(e) => e.preventDefault()} onDrop={(e) => { e.preventDefault(); void addImages(Array.from(e.dataTransfer.files)) }} className={cn("shrink-0 px-2.5 pt-2 pb-[max(0.5rem,var(--safe-bottom))]", !p.floating && "border-t bg-background")}>
      {trigger && pickerItems.length ? (
        <div className="mb-2 max-h-56 overflow-y-auto rounded-[22px] border bg-card p-1.5 shadow-[0_10px_30px_-14px_rgb(0_0_0/0.45)]" role="listbox" aria-label={trigger.kind === "skill" ? "Skills" : "Commands"}>
          {pickerItems.map((c) => (
            <button
              key={`${c.skill ? "$" : "/"}${c.name}`}
              role="option"
              aria-selected={false}
              className="flex min-h-10 w-full items-center gap-2 rounded-[16px] px-3 py-2 text-left text-[13.5px] active:bg-accent"
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => pickCommand(c, trigger)}
            >
              <span className="shrink-0 font-mono text-foreground">
                <span className={c.own ? "text-foreground" : "text-faint"}>{trigger.kind === "skill" || (c.skill && agentKind === "codex") ? "$" : "/"}</span>
                {c.name}
              </span>
              {c.hint ? <span className="max-w-28 shrink-0 truncate font-mono text-[11.5px] text-faint">{c.hint}</span> : null}
              <span className="min-w-0 flex-1 truncate text-[12px] text-muted-foreground">{c.description}</span>
              {c.skill ? <span className="shrink-0 rounded-full border px-1.5 text-[10px] text-faint">skill</span> : null}
            </button>
          ))}
        </div>
      ) : null}
      {images.length ? (
        <div className="mb-2 flex gap-2.5 overflow-x-auto px-1 pt-1.5">
          {images.map((im, i) => (
            <div key={i} className="relative size-16 shrink-0">
              <button className="size-full" aria-label={`View attachment ${i + 1}`} onClick={() => openImages(images.map((x, j) => ({ src: `data:${x.mime};base64,${x.data}`, name: `Attachment ${j + 1}` })), i)}>
                <img src={`data:${im.mime};base64,${im.data}`} alt={`Attachment ${i + 1}`} className="rounded-lg size-full border object-cover" />
              </button>
              <button className="rounded-full absolute -top-1 -right-1 grid size-7 place-items-center border bg-background" aria-label="Remove image" onClick={() => setImages((a) => a.filter((_, j) => j !== i))}>
                <X className="size-3" />
              </button>
            </div>
          ))}
        </div>
      ) : null}
      <div className={cn("rounded-[26px] border", p.sunk ? "bg-background" : "bg-card shadow-[0_10px_30px_-14px_rgb(0_0_0/0.45)]", p.goalActive && "border-primary/25")}>
        {p.goalHeader}
        <textarea
          data-composer
          ref={box}
          rows={1}
          value={text}
          placeholder={p.placeholder ?? "Message"}
          onChange={(e) => {
            setText(e.target.value)
            updateTrigger(e.target.value, e.target.selectionStart ?? e.target.value.length)
          }}
          onBlur={() => setTimeout(() => setTrigger(null), 150)}
          onPaste={(e) => {
            const files = Array.from(e.clipboardData.files)
            if (files.length) { e.preventDefault(); void addImages(files) }
          }}
          className="block max-h-[150px] min-h-11 w-full resize-none bg-transparent px-3.5 pt-3 pb-1 text-[15px] leading-snug outline-none placeholder:text-faint"
          aria-label="Message"
        />
        <input
          ref={file}
          type="file"
          accept="image/png,image/jpeg,image/gif,image/webp,.png,.jpg,.jpeg,.gif,.webp"
          multiple
          className="hidden"
          onChange={async (e) => {
            const files = [...(e.target.files ?? [])]
            e.target.value = ""
            await addImages(files)
          }}
        />
        <div className="flex items-center gap-1 p-1">
          <button className="rounded-full grid size-10 shrink-0 place-items-center text-muted-foreground active:bg-accent" aria-label="Attach, commands, effort, permissions and context" onClick={() => setSheet("more")}>
            <Plus className="size-5" />
          </button>
          <button
            className="rounded-full flex h-10 min-w-10 shrink items-center justify-center gap-1.5 px-3 text-[13px] text-muted-foreground active:bg-accent"
            aria-label="Model"
            onClick={() => (setSheet("model"), p.onModelsOpen?.())}
          >
            <Cpu className="size-4 shrink-0" />
            {p.running && has ? null : (
              <>
                <span className="truncate">{current?.name ?? (p.model || "Model")}</span>
                <ChevronDown className="size-3.5 shrink-0" />
              </>
            )}
          </button>
          <span className="flex-1" />
          {p.running && p.onStop && !p.goalActive ? (
            <button className="rounded-full grid size-10 shrink-0 place-items-center border border-input active:bg-accent" aria-label="Stop the turn" onClick={p.onStop}>
              <Square className="size-3.5 fill-current" />
            </button>
          ) : null}
          {p.running && has ? (
            <button className="rounded-full flex h-10 shrink-0 items-center gap-1 border border-input px-3 text-[13px] active:bg-accent" disabled={p.disabled} aria-label="Send now, into the running turn" onClick={() => send("now")}>
              <Zap className="size-4" /> Now
            </button>
          ) : null}
          {p.goalActive || (p.running && has) ? (
            <button
              className={cn(
                "flex h-10 shrink-0 items-center gap-1.5 px-3 text-[13px] font-medium disabled:opacity-60",
                p.goalActive ? "rounded-full border border-input bg-card text-foreground active:bg-accent" : "rounded-full bg-primary text-primary-foreground",
              )}
              disabled={!has || p.disabled}
              aria-label={p.goalActive ? "Queue reply" : "Queue to send when the turn ends"}
              onClick={() => send("queue")}
            >
              {p.goalActive ? <>Queue<span className="hidden min-[380px]:inline">reply</span></> : <><ListPlus className="size-4" /> Queue</>}
            </button>
          ) : p.running ? null : (
            <button
              className="rounded-full grid size-10 shrink-0 place-items-center bg-primary text-primary-foreground disabled:bg-muted disabled:text-faint"
              aria-label="Send"
              disabled={!has || p.disabled}
              onClick={() => send("queue")}
            >
              <ArrowUp className="size-5" />
            </button>
          )}
        </div>
      </div>

      <PullSheet open={sheet === "more"} onClose={() => setSheet(null)} title="Chat options" header={<SheetHead title="Chat options" />}>
        <div className="min-h-0 overflow-y-auto px-3 pb-2">
          <div className="grid grid-cols-2 gap-1.5">
            <button
              className="flex h-12 items-center justify-center gap-2 rounded-full bg-muted text-[14px] active:bg-accent"
              onClick={() => {
                // Opens the picker while the tap still counts as one, then closes the sheet.
                file.current?.click()
                setSheet(null)
              }}
            >
              <ImagePlus className="size-[18px] text-muted-foreground" /> Images
            </button>
            <button
              className="flex h-12 items-center justify-center gap-2 rounded-full bg-muted text-[14px] active:bg-accent"
              onClick={() => {
                p.onModelsOpen?.()
                setSheet("commands")
              }}
            >
              <Slash className="size-[18px] text-muted-foreground" /> Commands
            </button>
          </div>
          {p.onModel && current?.efforts.length ? (
            <>
              <Heading>Effort</Heading>
              <div className="flex gap-1 rounded-full bg-muted p-1" role="group" aria-label="Effort">
                {[DEFAULT_EFFORT, ...current.efforts].map((e) => {
                  const on = (p.effort || DEFAULT_EFFORT) === e
                  return (
                    <button key={e} aria-pressed={on} className={cn("h-10 min-w-0 flex-1 truncate rounded-full px-2 text-[13px]", on ? "bg-primary font-semibold text-primary-foreground" : "active:bg-accent")} onClick={() => p.onModel?.(p.model, e === DEFAULT_EFFORT ? "" : e)}>
                      {e === DEFAULT_EFFORT ? "Default" : effortLabel(e)}
                    </button>
                  )
                })}
              </div>
            </>
          ) : null}
          {p.onPermission ? (
            <>
              <Heading>Permissions</Heading>
              <div className="grid grid-cols-4 gap-1.5" role="group" aria-label="Permissions">
                {MODES.map((m) => {
                  const Icon = PERMISSION_ICON[m]
                  const on = p.permission === m
                  return (
                    <button key={m} aria-pressed={on} className={cn("flex h-[72px] flex-col items-center justify-center gap-1.5 rounded-[20px] bg-muted px-1 text-[11.5px] leading-tight active:bg-accent", on && "ring-2 ring-primary")} onClick={() => void p.onPermission?.(m)}>
                      <Icon className={cn("size-5", PERMISSION_TONE[m] || "text-muted-foreground")} />
                      {PERMISSION_LABEL[m]}
                    </button>
                  )
                })}
              </div>
            </>
          ) : null}
          {p.onCompact || p.onClear ? (
            <>
              <Heading>Context</Heading>
              <div className="rounded-[22px] bg-muted p-2">
                {p.context && p.context.window > 0 ? (
                  <div className="px-2 pt-1 pb-2.5">
                    <div className="mb-1.5 flex text-[12.5px]">
                      <span className="flex-1">{Math.min(100, Math.round((p.context.used / p.context.window) * 100))}% used</span>
                      <span className="font-mono text-muted-foreground">{formatTokens(p.context.used)} / {formatTokens(p.context.window)}</span>
                    </div>
                    <div className="h-1.5 overflow-hidden rounded-full bg-background">
                      <div className="h-full rounded-full bg-primary" style={{ width: `${Math.min(100, Math.round((p.context.used / p.context.window) * 100))}%` }} />
                    </div>
                  </div>
                ) : null}
                <div className="flex gap-1.5">
                  {p.onCompact ? (
                    <button className="h-10 flex-1 rounded-full bg-background text-[13px] active:bg-accent" onClick={() => (p.onCompact?.(""), setSheet(null))}>
                      Compact
                    </button>
                  ) : null}
                  {p.onClear ? (
                    <button
                      className={cn("h-10 flex-1 rounded-full bg-background text-[13px] text-red active:bg-red/10", clearArmed && "bg-red/10")}
                      onClick={() => {
                        // Clearing can't be undone: the first tap asks, the second clears.
                        if (!clearArmed) return setClearArmed(true)
                        p.onClear?.()
                        setSheet(null)
                      }}
                    >
                      {clearArmed ? "Tap again to clear" : "Clear"}
                    </button>
                  ) : null}
                </div>
              </div>
            </>
          ) : null}
        </div>
      </PullSheet>

      <PullSheet open={sheet === "commands"} onClose={() => setSheet(null)} title="Commands" header={<SheetHead title="Commands" />}>
        <div className="min-h-0 overflow-y-auto px-3 pb-2">
          <div className="flex flex-col gap-1">
            {allCommands.map((c) => (
              <button
                key={`${c.skill ? "$" : "/"}${c.name}`}
                className="flex w-full items-center gap-3 rounded-[20px] bg-muted/50 px-4 py-3 text-left active:bg-accent"
                onClick={() => pickCommand(c, null)}
              >
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5 truncate font-mono text-[14px]">
                    <span>
                      <span className={c.own ? "text-foreground" : "text-faint"}>{c.skill && agentKind === "codex" ? "$" : "/"}</span>
                      {c.name}
                    </span>
                    {c.hint ? <span className="truncate text-[12px] text-faint">{c.hint}</span> : null}
                  </div>
                  {c.description ? <div className="truncate text-[12px] text-muted-foreground">{c.description}</div> : null}
                </div>
                {c.skill ? <span className="shrink-0 rounded-full border px-2 py-0.5 text-[10.5px] text-faint">skill</span> : null}
              </button>
            ))}
          </div>
        </div>
      </PullSheet>

      <PullSheet open={sheet === "model"} onClose={() => setSheet(null)} title="Model" header={<SheetHead title="Model" />}>
        <div className="min-h-0 overflow-y-auto px-3 pb-2">
          {current && typeof current.fastMode === "boolean" && (
            <Switch className="mb-2 flex w-full rounded-[20px] bg-muted px-4 py-2.5 text-sm" aria-label="Fast mode" checked={p.fastMode ?? current.fastMode} onCheckedChange={(fast) => p.onModel?.(current.id, p.effort, fast)}>
              <span className="flex items-center gap-2"><Zap className="size-4 text-muted-foreground" /><span>Fast mode<span className="block text-xs text-muted-foreground">Uses more credits or quota</span></span></span>
            </Switch>
          )}
          {p.models.length === 0 ? (
            <div className="px-1 py-2 text-[13px] leading-relaxed text-muted-foreground">
              {p.modelsFailed ? (p.modelsError || "The chat uses the default model. Reopen this menu to try loading the model list again.") : "Loading models…"}
            </div>
          ) : null}
          <div className="flex flex-col gap-1">
            {p.models.map((m) => {
              const on = m.id === p.model
              return (
                <button
                  key={m.id}
                  className={cn("flex w-full items-center gap-3 rounded-[20px] px-4 py-3 text-left active:bg-accent", on ? "bg-muted ring-2 ring-primary" : "bg-muted/50")}
                  onClick={() => (p.onModel?.(m.id, m.efforts.includes(p.effort) ? p.effort : ""), setSheet(null))}
                >
                  <div className="min-w-0 flex-1">
                    <div className={cn("truncate text-[14.5px]", on && "font-semibold")}>{m.name}</div>
                    {m.description ? <div className="truncate text-[12px] text-muted-foreground">{m.description}</div> : null}
                  </div>
                  {on ? <Check className="size-4 shrink-0" /> : null}
                </button>
              )
            })}
          </div>
        </div>
      </PullSheet>
    </div>
  )
}
