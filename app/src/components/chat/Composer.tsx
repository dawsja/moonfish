import { readGoalCommand, type GoalInput } from "@/lib/goals"
import { useEffect, useRef, useState, type ReactNode } from "react"
import { toast } from "sonner"
import { ArrowUp, Check, ChevronDown, File, Paperclip, Square, X } from "lucide-react"
import { Switch } from "@/components/ui/switch"
import { Tip } from "@/components/Tip"
import { openImages } from "@/components/ImageView"
import { AgentGlyph } from "@/components/icons"
import { promptText } from "@/components/Prompt"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { effortLabel, type ContextFill, type ModelOption, type SlashCommand } from "@/lib/chat"
import { ContextRing } from "./ContextRing"
import { filterCommands, insertSkill, menuCommands, ownCommand, skillCommands, skillQuery, slashQuery, type MenuCommand, type OwnCommand } from "@/lib/commands"
import { fuzzyPaths, mention, mentionAt } from "@/lib/fuzzy"
import { git } from "@/lib/git"
import { attachmentMime, readAttachment, MAX_IMAGE, type Attachment } from "@/lib/attachments"
import { AGENT_LABEL, PERMISSION_LABEL, type AgentKind, type PermissionMode } from "@/lib/types"
import { PERMISSION_ICON, PERMISSION_TONE } from "@/components/permission"
import { useScope } from "@/store/git"
import type { InsertDetail } from "@/store/insert"
import { drafts } from "@/store/drafts"
import { cn } from "@/lib/utils"
import { PromptHistory, historyBoundary } from "@/lib/prompt-history"

export type ComposerProps = {
  agent: AgentKind
  /** Agents to pick from (a new chat). */
  agents?: AgentKind[]
  onAgent?: (a: AgentKind) => void
  model: string
  /** Reasoning effort; empty: the model's default. */
  effort?: string
  fastMode?: boolean
  /** How full the chat's context is (an open chat). */
  context?: ContextFill | null
  /** The agent's models; empty while unknown. */
  models?: ModelOption[]
  modelsError?: string | null
  /** Picks a model and its effort (empty: the model's default). */
  onModel?: (id: string, effort: string, fastMode?: boolean) => void
  /** Called when the model menu or the / menu opens, to load the agent's lists. */
  onModelsOpen?: () => void
  /** The agent's slash commands (the / menu adds Moonfish's own). */
  commands?: SlashCommand[]
  /** `/clear` and `/compact`, in a chat that has started. */
  onClear?: () => void
  onCompact?: (instructions: string) => void
  onGoal?: (input: GoalInput) => Promise<boolean>
  goalHeader?: ReactNode
  goalActive?: boolean
  permission: PermissionMode
  onPermission?: (p: PermissionMode) => void | Promise<void>
  running?: boolean
  disabled?: boolean
  placeholder?: string
  /** `steer` (Ctrl/⌘+Enter): send it now, into the turn that's running. */
  /** Resolves false when the message didn't go: the composer puts it back. */
  onSend?: (text: string, images: Attachment[], how: "send" | "steer", permissionOverride?: PermissionMode) => void | Promise<boolean | void>
  onStop?: () => void
  /** Ctrl/⌘+Enter with nothing typed while the agent works: the first queued message goes now. */
  onSteerQueued?: () => void
  /** Where what's typed is kept until it's sent (see `draftKey`); mount one composer per key. */
  history?: string[]
  draftKey?: string
  className?: string
  autoFocus?: boolean
  /** The agent can't change once a chat started. */
  lockAgent?: boolean
}

const PERMISSIONS: PermissionMode[] = ["ask", "acceptEdits", "plan", "fullAccess"]

export type { Attachment } from "@/lib/attachments"
const MAC = /Mac/.test(navigator.userAgent)
/** The send-now chord's modifier, as shown. */
export const STEER_KEY = MAC ? "⌘" : "Ctrl"

/** Puts a message back in the composer to edit (a queued one taken off the queue). */
export function editInComposer(text: string, images: Attachment[]) {
  window.dispatchEvent(new CustomEvent<EditDetail>("moonfish:edit", { detail: { text, images } }))
}
type EditDetail = { text: string; images: Attachment[] }
const MAX_IMAGES = 8

/** Files for the @ menu, per folder, kept briefly. */
const fileCache = new Map<string, { at: number; files: Promise<string[]> }>()

/** The prompt box: text, then a toolbar with the agent, model and permission chips. */
export function Composer(p: ComposerProps) {
  const history = useRef(new PromptHistory())
  const seenHistory = useRef(0)
  useEffect(() => {
    const entries = p.history ?? []
    for (const entry of entries.slice(seenHistory.current)) history.current.add(entry)
    seenHistory.current = entries.length
  }, [p.history])
  const remember = (value: string) => {
    history.current.add(value)
    history.current.reset()
    setSlash(null)
    setMenu(null)
  }
  const PermissionIcon = PERMISSION_ICON[p.permission]
  const scope = useScope()
  const [menu, setMenu] = useState<{ start: number; query: string; items: string[]; index: number } | null>(null)
  const updateMenu = async (value: string, caret: number) => {
    const m = mentionAt(value, caret)
    if (!m || !scope) return setMenu(null)
    const key = `${scope.projectId}|${scope.cwd}`
    let entry = fileCache.get(key)
    if (!entry || Date.now() - entry.at > 30_000) {
      entry = { at: Date.now(), files: git.files(scope).catch(() => [] as string[]) }
      fileCache.set(key, entry)
    }
    const items = fuzzyPaths(await entry.files, m.query, 20)
    setMenu(items.length ? { start: m.start, query: m.query, items, index: 0 } : null)
  }
  const pick = (path: string) => {
    if (!menu) return
    const el = ref.current
    const end = menu.start + 1 + menu.query.length
    const insert = mention(path) + " "
    setText((t) => t.slice(0, menu.start) + insert + t.slice(end))
    setMenu(null)
    requestAnimationFrame(() => {
      el?.focus({ preventScroll: true })
      el?.setSelectionRange(menu.start + insert.length, menu.start + insert.length)
    })
  }
  const inChat = !!p.onClear
  const [slashState, setSlash] = useState<{ kind: "slash" | "skill"; start: number; query: string; index: number } | null>(null)
  const [dismissedAtPromptText, setDismissedAtPromptText] = useState<string | null>(null)
  const updateSlash = (value: string, caret: number) => {
    if (dismissedAtPromptText === value) return setSlash(null)
    const q = slashQuery(value, caret)
    if (q !== null) {
      if (q === "") p.onModelsOpen?.()
      setSlash({ kind: "slash", start: 0, query: q, index: 0 })
      return
    }
    const sq = skillQuery(value, caret)
    if (sq !== null) {
      if (sq.query === "") p.onModelsOpen?.()
      setSlash({ kind: "skill", start: sq.start, query: sq.query, index: 0 })
      return
    }
    setSlash(null)
  }
  // Worked out on each render, so the list fills in when the agent's commands arrive.
  const slashItems = slashState
    ? filterCommands(
        slashState.kind === "skill" ? skillCommands(p.commands ?? []) : menuCommands(p.agent, p.commands ?? [], inChat),
        slashState.query,
      ).slice(0, 60)
    : []
  const slash = slashState && slashItems.length ? { items: slashItems, index: Math.min(slashState.index, slashItems.length - 1), kind: slashState.kind } : null
  const [modelOpen, setModelOpen] = useState(false)
  const runOwn = (own: OwnCommand, args: string, how: "send" | "steer" = "send", sent: Attachment[] = [], rawText = "") => {
    if (own === "model") {
      if (!args) {
        p.onModelsOpen?.()
        setModelOpen(true)
        return
      }
      const a = args.toLowerCase()
      const m =
        p.models?.find((x) => x.id.toLowerCase() === a || x.name.toLowerCase() === a) ??
        p.models?.find((x) => x.id.toLowerCase().includes(a) || x.name.toLowerCase().includes(a))
      p.onModel?.(m?.id ?? args, p.effort ?? "")
      toast.success(`Model set to ${m?.name ?? args}`)
    } else if (own === "plan") {
      if (!args) {
        void p.onPermission?.("plan")
        toast.success("The agent will propose a plan before making changes")
        return
      }
      void (async () => {
        if (p.permission !== "plan") toast.success("The agent will propose a plan before making changes")
        await p.onPermission?.("plan")
        const ok = await p.onSend?.(args, sent, how, "plan")
        if (ok !== false || !mounted.current) return
        setText((cur) => (cur.trim() ? cur : rawText))
        setImages((cur) => (cur.length ? cur : sent))
      })()
    } else if (own === "clear") p.onClear?.()
    else if (own === "compact") p.onCompact?.(args)
    else if (own === "goal") void runGoal(args)
  }
  const runGoal = async (args: string) => {
    const command = readGoalCommand(`/goal ${args}`)
    if (command && await p.onGoal?.(command) === false && mounted.current) {
      setText((current) => current.trim() || `/goal ${args}`.trim())
    }
  }
  const pickCommand = (c: MenuCommand, run: boolean) => {
    const el = ref.current
    const active = slashState
    setSlash(null)
    if (active?.kind === "skill") {
      const caret = active.start + c.name.length + 2
      setText((t) => insertSkill(t, active, c.name).text)
      requestAnimationFrame(() => {
        el?.focus({ preventScroll: true })
        el?.setSelectionRange(caret, caret)
      })
      return
    }
    if (run && c.own) {
      remember(`/${c.name}`)
      setText("")
      runOwn(c.own, "")
      return
    }
    if (run && !c.skill && !c.hint) {
      remember(`/${c.name}`)
      setText("")
      p.onSend?.(`/${c.name}`, [], "send")
      return
    }
    // Codex skills are mentioned with $ in the message; they don't run by themselves.
    const insert = c.skill && p.agent === "codex" ? `$${c.name} ` : `/${c.name} `
    setText(insert)
    requestAnimationFrame(() => {
      el?.focus({ preventScroll: true })
      el?.setSelectionRange(insert.length, insert.length)
    })
  }
  const [text, setText] = useState(() => (p.draftKey ? drafts.get(p.draftKey).text : ""))
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => void (mounted.current = false)
  }, [])
  const [images, setImages] = useState<Attachment[]>(() => (p.draftKey ? drafts.get(p.draftKey).images : []))
  useEffect(() => {
    if (p.draftKey) drafts.set(p.draftKey, text, images)
  }, [p.draftKey, text, images])
  const [dragging, setDragging] = useState(false)
  const ref = useRef<HTMLTextAreaElement>(null)
  // Grows with the text up to its max height (then scrolls), and shrinks back after sending.
  useEffect(() => {
    const el = ref.current
    if (!el) return
    el.style.height = "auto"
    el.style.height = `${el.scrollHeight}px`
  }, [text])
  useEffect(() => {
    if (p.autoFocus) ref.current?.focus({ preventScroll: true })
  }, [p.autoFocus])
  const picker = useRef<HTMLInputElement>(null)
  const addFiles = async (files: Iterable<File>) => {
    const all = [...files]
    const tooBig = all.filter((f) => attachmentMime(f) && f.size > MAX_IMAGE)
    if (tooBig.length) toast.error(`${tooBig.map((f) => f.name || "That image").join(", ")} is over 5 MB`)
    if (all.some((f) => !attachmentMime(f))) toast.info("Attach a PNG, JPEG, GIF, or WebP image")
    const results = await Promise.allSettled(all.filter((f) => attachmentMime(f) && f.size <= MAX_IMAGE).map(readAttachment))
    const read = results.flatMap((r) => {
      if (r.status === "fulfilled") return [r.value]
      toast.error(String(r.reason))
      return []
    })
    if (read.length) setImages((cur) => [...cur, ...read].slice(0, MAX_IMAGES))
  }
  /** Puts `add` at the cursor: a multiline block on a line of its own, a mention or word after a space. */
  const insert = (add: string) => {
    const el = ref.current
    const at = el?.selectionStart ?? text.length
    setText((t) => {
      const before = t.slice(0, at)
      const sep = !before ? "" : add.includes("\n") ? (before.endsWith("\n") ? "" : "\n") : /\s$/.test(before) ? "" : " "
      return before + sep + add + t.slice(at)
    })
    requestAnimationFrame(() => el?.focus({ preventScroll: true }))
  }
  // "Add to chat" from the side panel puts `@path ` at the cursor.
  useEffect(() => {
    const onInsert = (e: Event) => {
      const { text: add } = (e as CustomEvent<InsertDetail>).detail
      insert(add)
    }
    window.addEventListener("moonfish:insert", onInsert)
    return () => window.removeEventListener("moonfish:insert", onInsert)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text.length])
  // A queued message back to edit: it goes before whatever is typed already.
  useEffect(() => {
    const onEdit = (e: Event) => {
      const back = (e as CustomEvent<EditDetail>).detail
      setText((t) => (t.trim() ? `${back.text}\n\n${t}` : back.text))
      setImages((cur) => [...back.images, ...cur].slice(0, MAX_IMAGES))
      requestAnimationFrame(() => {
        const el = ref.current
        el?.focus({ preventScroll: true })
        el?.setSelectionRange(back.text.length, back.text.length)
      })
    }
    window.addEventListener("moonfish:edit", onEdit)
    return () => window.removeEventListener("moonfish:edit", onEdit)
  }, [])
  const send = (how: "send" | "steer" = "send") => {
    const t = text.trim()
    if (!t && !images.length && how === "steer" && p.running) return p.onSteerQueued?.()
    if ((!t && !images.length) || p.disabled) return
    remember(t)
    const own = ownCommand(t, inChat, p.agent)
    if (own && (!images.length || (own.own === "plan" && !!own.args))) {
      const sent = own.own === "plan" && own.args ? images : []
      runOwn(own.own, own.args, how, sent, t)
      setText("")
      if (sent.length) setImages([])
      return
    }
    const sent = images
    void Promise.resolve(p.onSend?.(t || "(see the attached image)", sent, how)).then((ok) => {
      // Didn't go: back in the box, unless something new was typed meanwhile.
      if (ok !== false || !mounted.current) return
      setText((cur) => (cur.trim() ? cur : t))
      setImages((cur) => (cur.length ? cur : sent))
    })
    setText("")
    setImages([])
  }

  return (
    <div
      className={cn("@container rounded-2xl border border-border bg-card shadow-[0_6px_20px_-12px_rgb(15_23_42/0.25)] dark:shadow-[0_1px_0_rgb(255_255_255/0.03)_inset,0_8px_24px_-12px_rgb(0_0_0/0.6)] focus-within:border-ring/70", p.goalActive && "border-primary/25", dragging && "border-primary", p.className)}
      onDragOver={(e) => {
        if ([...e.dataTransfer.items].some((i) => i.kind === "file")) {
          e.preventDefault()
          setDragging(true)
        }
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(e) => {
        setDragging(false)
        if (e.dataTransfer.files.length) {
          e.preventDefault()
          addFiles(e.dataTransfer.files)
        }
      }}
    >
      {p.goalHeader}
      {images.length ? (
        <div className="flex flex-wrap gap-1.5 px-3 pt-3">
          {images.map((img, i) => (
            <div key={i} className="group/img relative size-14">
              <button className="rounded-lg size-full overflow-hidden border bg-background" aria-label={`View ${img.name}`} onClick={() => openImages(images.map((x) => ({ src: `data:${x.mime};base64,${x.data}`, name: x.name })), i)}>
                <img src={`data:${img.mime};base64,${img.data}`} alt={img.name} className="size-full object-cover" />
              </button>
              <button
                className="rounded-full absolute -top-1.5 -right-1.5 z-10 hidden size-4 place-items-center bg-foreground text-background group-focus-within/img:grid group-hover/img:grid"
                aria-label="Remove image"
                onClick={() => setImages((cur) => cur.filter((_, j) => j !== i))}
              >
                <X className="size-3" />
              </button>
            </div>
          ))}
        </div>
      ) : null}
      {slash ? (
        <div className="relative">
          <div className="absolute bottom-0 left-2 z-20 mb-1 max-h-80 w-[min(36rem,calc(100%-1rem))] overflow-y-auto rounded-lg border bg-popover p-1 shadow-xl" onMouseDown={(e) => e.preventDefault()} role="listbox" aria-label={slash.kind === "skill" ? "Skills" : "Commands"}>
            {slash.items.map((c, i) => (
              <button
                key={`${c.skill ? "$" : "/"}${c.name}`}
                ref={i === slash.index ? (el) => el?.scrollIntoView({ block: "nearest" }) : undefined}
                role="option"
                aria-selected={i === slash.index}
                className={cn("flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[12.5px]", i === slash.index ? "bg-accent text-foreground" : "text-muted-foreground")}
                onMouseEnter={() => setSlash({ ...slashState!, index: i })}
                onClick={() => pickCommand(c, true)}
              >
                {/* The prefix as you'd type it, joined to the name: a slash icon slants far more than the font's "/". */}
                <span className="shrink-0 font-mono text-foreground">
                  <span className={c.own ? "text-foreground" : "text-faint"}>{slash.kind === "skill" || (c.skill && p.agent === "codex") ? "$" : "/"}</span>
                  {c.name}
                </span>
                {c.hint ? <span className="max-w-40 shrink-0 truncate font-mono text-[11px] text-faint">{c.hint}</span> : null}
                <span className="min-w-0 flex-1 truncate text-[11.5px] text-faint">{c.description}</span>
                {c.skill ? <span className="shrink-0 rounded-full border px-1.5 text-[10px] text-faint">skill</span> : null}
              </button>
            ))}
          </div>
        </div>
      ) : null}
      {menu ? (
        <div className="relative">
          <div className="absolute bottom-0 left-2 z-20 mb-1 max-h-72 w-[min(32rem,calc(100%-1rem))] overflow-y-auto rounded-lg border bg-popover p-1 shadow-xl" onMouseDown={(e) => e.preventDefault()}>
            {menu.items.map((f, i) => {
              const slash = f.lastIndexOf("/")
              return (
                <button key={f} className={cn("flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-[12.5px]", i === menu.index ? "bg-accent text-foreground" : "text-muted-foreground")} onMouseEnter={() => setMenu({ ...menu, index: i })} onClick={() => pick(f)}>
                  <File className="size-3.5 shrink-0 text-faint" />
                  <span className="truncate text-foreground">{f.slice(slash + 1)}</span>
                  <span className="min-w-0 truncate text-[11px] text-faint">{slash > 0 ? f.slice(0, slash) : ""}</span>
                </button>
              )
            })}
          </div>
        </div>
      ) : null}
      <input ref={picker} type="file" accept="image/png,image/jpeg,image/gif,image/webp,.png,.jpg,.jpeg,.gif,.webp" multiple hidden onChange={(e) => {
        if (e.target.files) void addFiles(e.target.files)
        e.target.value = ""
      }} />
      <textarea
        data-composer
        aria-label="Message"
        ref={ref}
        value={text}
        rows={3}
        placeholder={p.placeholder ?? "Send a message. Type @ for files or / for commands."}
        className="block max-h-72 min-h-[76px] w-full resize-none bg-transparent px-4 pt-3.5 text-[length:var(--moonfish-chat-size,14px)] leading-relaxed outline-none placeholder:text-faint"
        onChange={(e) => {
          setText(e.target.value)
          const caret = e.target.selectionStart ?? e.target.value.length
          updateMenu(e.target.value, caret)
          updateSlash(e.target.value, caret)
        }}
        onBlur={() =>
          setTimeout(() => {
            setMenu(null)
            setSlash(null)
          }, 150)
        }
        onPaste={(e) => {
          const files = [...e.clipboardData.files].filter((f) => attachmentMime(f))
          if (files.length) {
            e.preventDefault()
            addFiles(files)
          }
        }}
        onKeyDown={(e) => {
          // An approval or question card above took this key.
          if (e.defaultPrevented) return
          if (slash && !e.nativeEvent.isComposing) {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault()
              const d = e.key === "ArrowDown" ? 1 : -1
              setSlash({ ...slashState!, index: (slash.index + d + slash.items.length) % slash.items.length })
              return
            }
            if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey) {
              e.preventDefault()
              pickCommand(slash.items[slash.index], e.key === "Enter")
              return
            }
            if (e.key === "Escape") {
              e.preventDefault()
              e.stopPropagation()
              setSlash(null)
              setDismissedAtPromptText(text)
              return
            }
          }
          if (menu && !e.nativeEvent.isComposing) {
            if (e.key === "ArrowDown" || e.key === "ArrowUp") {
              e.preventDefault()
              const d = e.key === "ArrowDown" ? 1 : -1
              setMenu({ ...menu, index: (menu.index + d + menu.items.length) % menu.items.length })
              return
            }
            if ((e.key === "Enter" || e.key === "Tab") && !e.shiftKey) {
              e.preventDefault()
              pick(menu.items[menu.index])
              return
            }
            if (e.key === "Escape") {
              e.preventDefault()
              e.stopPropagation()
              setMenu(null)
              return
            }
          }
          if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.nativeEvent.isComposing && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
            const direction = e.key === "ArrowUp" ? -1 : 1
            const el = e.currentTarget
            if (historyBoundary(text, el.selectionStart, el.selectionEnd, direction)) {
              const recalled = history.current.move(direction, text)
              if (recalled !== null) {
                e.preventDefault()
                setText(recalled)
                // A recalled /command is text to resend, not a fresh autocomplete query.
                setSlash(null)
                setMenu(null)
                const caret = direction === -1 ? 0 : recalled.length
                requestAnimationFrame(() => el.setSelectionRange(caret, caret))
                return
              }
            }
          }
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault()
            send((MAC ? e.metaKey : e.ctrlKey) ? "steer" : "send")
          } else if (e.key === "Escape" && p.running) {
            e.preventDefault()
            p.onStop?.()
          }
        }}
      />
      {/* In a narrow pane the agent is its glyph alone, then the model's name gives way. */}
      <div className="flex min-w-0 items-center gap-1 px-2 pb-2">
        <DropdownMenu>
          <DropdownMenuTrigger asChild disabled={p.lockAgent}>
            <button className="rounded-full flex h-7 shrink-0 items-center gap-1.5 px-2.5 text-[12px] font-medium whitespace-nowrap hover:bg-accent disabled:hover:bg-transparent">
              <AgentGlyph agent={p.agent} className="size-3.5" />
              <span className="@max-[24rem]:sr-only">{AGENT_LABEL[p.agent]}</span>
              {p.lockAgent ? null : <ChevronDown className="size-3 text-muted-foreground" />}
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-56">
            <DropdownMenuLabel>Agent</DropdownMenuLabel>
            <DropdownMenuRadioGroup value={p.agent} onValueChange={(v) => p.onAgent?.(v as AgentKind)}>
              {(p.agents ?? [p.agent]).map((a) => (
                <DropdownMenuRadioItem key={a} value={a}>
                  <AgentGlyph agent={a} className="size-3.5" /> {AGENT_LABEL[a]}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
        <DropdownMenu
          open={modelOpen}
          onOpenChange={(o) => {
            setModelOpen(o)
            if (o) p.onModelsOpen?.()
          }}
        >
          <DropdownMenuTrigger asChild disabled={!p.onModel}>
            <button className="rounded-full flex h-7 max-w-56 min-w-0 items-center gap-1 px-2.5 font-mono text-[11.5px] whitespace-nowrap text-muted-foreground hover:bg-accent disabled:hover:bg-transparent">
              <span className="truncate">{p.models?.find((m) => m.id === p.model)?.name ?? (p.model || "default model")}</span>
              {p.effort ? <span className="shrink-0 text-faint">· {effortLabel(p.effort)}</span> : null}
              {(p.fastMode ?? p.models?.find((m) => m.id === p.model)?.fastMode) && <span className="shrink-0">· Fast</span>}
              {p.onModel ? <ChevronDown className="size-3 shrink-0" /> : null}
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="max-h-80 w-72 overflow-y-auto">
            <DropdownMenuLabel>Model{p.models?.some((m) => m.efforts?.length) ? <span className="ml-1.5 font-normal text-faint">Press Right Arrow for options</span> : null}</DropdownMenuLabel>
            <DropdownMenuItem onSelect={() => p.onModel?.("", "")}>
              <span className="w-3.5">{!p.model ? <Check className="size-3.5" /> : null}</span>
              The agent's default
            </DropdownMenuItem>
            {p.models === undefined || p.models.length === 0 ? (
              <div className="px-2 py-1.5 text-[11.5px] leading-relaxed text-faint">
                {p.models === undefined ? "Loading models…" : (p.modelsError || "The agent didn't list its models.")}
              </div>
            ) : null}
            {p.models?.map((m) =>
              m.efforts?.length || typeof m.fastMode === "boolean" ? (
                <ModelOptions key={m.id} model={m} selected={m.id === p.model} effort={m.id === p.model ? (p.effort ?? "") : ""} fastMode={m.id === p.model ? p.fastMode : undefined} onPick={(e, fast) => p.onModel?.(m.id, e, fast)} />
              ) : (
                <DropdownMenuItem key={m.id} onSelect={() => p.onModel?.(m.id, "")}>
                  <span className="w-3.5">{m.id === p.model ? <Check className="size-3.5" /> : null}</span>
                  <ModelText model={m} />
                </DropdownMenuItem>
              ),
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onSelect={async () => {
                const id = await promptText({ title: "Use a model", description: "Enter a model ID your agent supports.", value: p.model, placeholder: "Model ID", mono: true, confirm: "Use" })
                if (id) p.onModel?.(id, "")
              }}
            >
              <span className="w-3.5" /> Other model…
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <button className={cn("rounded-full flex h-7 shrink-0 items-center gap-1 px-2.5 text-[12px] whitespace-nowrap hover:bg-accent", PERMISSION_TONE[p.permission] || "text-muted-foreground")}>
              <PermissionIcon className="size-3.5" />
              {PERMISSION_LABEL[p.permission]}
              <ChevronDown className="size-3" />
            </button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-48">
            <DropdownMenuLabel>Permissions</DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuRadioGroup value={p.permission} onValueChange={(v) => p.onPermission?.(v as PermissionMode)}>
              {PERMISSIONS.map((m) => {
                const Icon = PERMISSION_ICON[m]
                return (
                  <DropdownMenuRadioItem key={m} value={m}>
                    <Icon className={cn("size-3.5", PERMISSION_TONE[m] || "text-muted-foreground")} />
                    <span className={cn(m === p.permission && PERMISSION_TONE[m])}>{PERMISSION_LABEL[m]}</span>
                  </DropdownMenuRadioItem>
                )
              })}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          <ContextRing context={p.context ?? null} />
          <Tip content="Attach up to 8 images of 5 MB each">
            <button className="rounded-full grid size-7 place-items-center text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Attach image" onClick={() => picker.current?.click()}>
              <Paperclip className="size-3.5" />
            </button>
          </Tip>
          {p.goalActive ? (
            <Tip content={p.running ? `Press Enter to queue. Press ${STEER_KEY}+Enter to send now.` : "Press Enter to queue."}>
              <button
                className="ml-1 flex h-7 items-center gap-1 rounded-md border border-input bg-card px-2 text-xs font-medium text-foreground hover:bg-accent disabled:text-faint"
                disabled={(!text.trim() && !images.length) || p.disabled}
                onClick={() => send()}
              >
                Queue reply
              </button>
            </Tip>
          ) : p.running && (text.trim() || images.length) ? (
            <Tip content={`Press Enter to queue. Press ${STEER_KEY}+Enter to send now.`}>
              <button className="rounded-full ml-1 grid size-7 place-items-center bg-primary text-primary-foreground hover:bg-primary/90" aria-label="Queue" onClick={() => send()}>
                <ArrowUp className="size-4" />
              </button>
            </Tip>
          ) : null}
          {p.goalActive ? null : p.running ? (
            <Tip content="Press Esc to stop">
              <button className="rounded-full ml-1 grid size-7 place-items-center bg-foreground text-background hover:opacity-90" aria-label="Stop" onClick={p.onStop}>
                <Square className="size-3 fill-current" />
              </button>
            </Tip>
          ) : (
            <Tip content="Press Enter to send">
              <button
                className="rounded-full ml-1 grid size-7 place-items-center bg-primary text-primary-foreground hover:bg-primary/90 disabled:bg-muted disabled:text-faint"
                aria-label="Send"
                disabled={(!text.trim() && !images.length) || p.disabled}
                onClick={() => send()}
              >
                <ArrowUp className="size-4" />
              </button>
            </Tip>
          )}
        </div>
      </div>
    </div>
  )
}

function ModelText({ model }: { model: ModelOption }) {
  return (
    <span className="min-w-0 flex-1">
      <span className="block truncate">{model.name}</span>
      {model.description ? <span className="block truncate text-[11px] text-faint">{model.description}</span> : null}
    </span>
  )
}

/** Model-specific reasoning and speed options. */
function ModelOptions({ model, selected, effort, fastMode, onPick }: { model: ModelOption; selected: boolean; effort: string; fastMode?: boolean; onPick: (effort: string, fastMode?: boolean) => void }) {
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger className="gap-2">
        <span className="w-3.5">{selected ? <Check className="size-3.5" /> : null}</span>
        <ModelText model={model} />
        {selected && effort ? <span className="shrink-0 text-[11px] text-muted-foreground">{effortLabel(effort)}</span> : null}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="w-48">
        <DropdownMenuLabel className="text-[11px] font-normal text-faint">{model.efforts.length ? model.name + " · effort" : model.name}</DropdownMenuLabel>
        <DropdownMenuItem onSelect={() => onPick("")}>
          <span className="w-3.5">{selected && !effort ? <Check className="size-3.5" /> : null}</span>
          <span className="flex-1">{model.efforts.length ? "Default" : "Use model"}</span>
          {model.defaultEffort ? <span className="text-[11px] text-faint">{effortLabel(model.defaultEffort)}</span> : null}
        </DropdownMenuItem>
        {model.efforts.length > 0 && <DropdownMenuSeparator />}
        {model.efforts.map((e) => (
          <DropdownMenuItem key={e} onSelect={() => onPick(e)}>
            <span className="w-3.5">{selected && effort === e ? <Check className="size-3.5" /> : null}</span>
            <span className="flex-1">{effortLabel(e)}</span>
            <EffortBars level={model.efforts.indexOf(e)} of={model.efforts.length} />
          </DropdownMenuItem>
        ))}
        {typeof model.fastMode === "boolean" && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem asChild role="switch" onSelect={(event) => event.preventDefault()}>
              <Switch className="w-full" aria-label="Fast mode" checked={fastMode ?? model.fastMode} onCheckedChange={(fast) => onPick(effort, fast)}>
                <span>Fast mode<span className="block text-[11px] font-normal text-muted-foreground">Uses more credits or quota</span></span>
              </Switch>
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  )
}

/** A small rising meter: how much thinking this level asks for. */
function EffortBars({ level, of }: { level: number; of: number }) {
  return (
    <span className="flex h-3 items-end gap-px" aria-hidden>
      {Array.from({ length: of }, (_, i) => (
        <span key={i} className={cn("w-[3px]", i <= level ? "bg-foreground/70" : "bg-muted")} style={{ height: `${4 + (8 * (i + 1)) / of}px` }} />
      ))}
    </span>
  )
}
