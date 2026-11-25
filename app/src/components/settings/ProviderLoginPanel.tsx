import { useCallback, useEffect, useRef, useState } from "react"
import { Check, Copy, ExternalLink, LoaderCircle, RefreshCw, X } from "lucide-react"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { copyText } from "@/lib/clipboard"
import { ipc, onLoginEvent } from "@/lib/ipc"
import { AGENT_LABEL, type Account, type AgentKind, type Location, type LoginEvent, type LoginMethod, type LoginStart } from "@/lib/types"
import { clearCatalogCache } from "@/components/chat/ChatView"
import { cn } from "@/lib/utils"
import { useApp } from "@/store/app"
import { useProviders } from "@/store/providers"
import { usageLocationKey, useUsage } from "@/store/usage"

export function ProviderLoginPanel({
  agent,
  location,
  onDone,
  onClose,
  onCatalogInvalidated,
  className,
}: {
  agent: AgentKind
  location: Location
  onDone: (account: Account) => void
  onClose: () => void
  onCatalogInvalidated?: (agent: AgentKind) => void
  className?: string
}) {
  const [session, setSession] = useState<LoginStart | null>(null)
  const [starting, setStarting] = useState(true)
  const [method, setMethod] = useState<LoginMethod>("auto")
  const [input, setInput] = useState("")
  const [submitting, setSubmitting] = useState(false)
  const [feedback, setFeedback] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [copiedUrl, setCopiedUrl] = useState(false)
  const [copiedCode, setCopiedCode] = useState(false)

  const sessionIdRef = useRef<string | null>(null)
  const activeRef = useRef(true)
  const runRef = useRef(0)
  const pendingEventsRef = useRef<LoginEvent[]>([])
  const locationRef = useRef(location)
  locationRef.current = location
  const onDoneRef = useRef(onDone)
  onDoneRef.current = onDone
  const onCloseRef = useRef(onClose)
  onCloseRef.current = onClose
  const onCatalogRef = useRef(onCatalogInvalidated)
  onCatalogRef.current = onCatalogInvalidated

  const locationKey = usageLocationKey(location)

  const openSignInUrl = useCallback((url: string) => {
    void import("@tauri-apps/plugin-opener")
      .then((o) => o.openUrl(url))
      .catch((e) => toast.error(`Could not open browser: ${String(e)}`))
  }, [])

  const handleEvent = useCallback(
    (event: LoginEvent) => {
      if (event.kind === "feedback") {
        setFeedback(event.message)
        setSubmitting(false)
        return
      }
      if (event.kind === "done") {
        sessionIdRef.current = null
        setSubmitting(false)
        const loc = locationRef.current
        useProviders.getState().invalidate(loc)
        void useProviders.getState().probeLocation(loc, useApp.getState().settings?.providers, true)
        useUsage.getState().invalidate(loc, agent)
        void useUsage.getState().load(loc, true)
        clearCatalogCache(agent)
        onCatalogRef.current?.(agent)
        toast.success(`Signed in to ${AGENT_LABEL[agent]}`, {
          description: event.account.label ?? undefined,
        })
        onDoneRef.current(event.account)
        onCloseRef.current()
        return
      }
      if (event.kind === "error") {
        sessionIdRef.current = null
        setSubmitting(false)
        setError(event.message)
      }
    },
    [agent],
  )

  const cancelActive = useCallback(() => {
    const id = sessionIdRef.current
    if (!id) return
    sessionIdRef.current = null
    void ipc.providerLoginCancel(id).catch(() => {})
  }, [])

  const start = useCallback(
    async (nextMethod: LoginMethod) => {
      cancelActive()
      const myRun = ++runRef.current
      pendingEventsRef.current = []
      setMethod(nextMethod)
      setStarting(true)
      setSession(null)
      setInput("")
      setSubmitting(false)
      setFeedback(null)
      setError(null)
      try {
        const res = await ipc.providerLoginStart(agent, locationRef.current, nextMethod)
        if (!activeRef.current || myRun !== runRef.current) {
          void ipc.providerLoginCancel(res.sessionId).catch(() => {})
          return
        }
        sessionIdRef.current = res.sessionId
        setSession(res)
        setStarting(false)
        if (!res.autoOpened && res.kind === "browser") {
          openSignInUrl(res.url)
        }
        const buffered = pendingEventsRef.current.filter((e) => e.sessionId === res.sessionId)
        pendingEventsRef.current = []
        for (const ev of buffered) handleEvent(ev)
      } catch (e) {
        if (!activeRef.current || myRun !== runRef.current) return
        setStarting(false)
        setError(String(e))
      }
    },
    [agent, cancelActive, handleEvent, openSignInUrl],
  )

  useEffect(() => {
    activeRef.current = true
    let unsub: (() => void) | null = null
    let cancelled = false
    void onLoginEvent((ev) => {
      if (!activeRef.current) return
      if (!sessionIdRef.current) {
        pendingEventsRef.current = [...pendingEventsRef.current.slice(-9), ev]
        return
      }
      if (ev.sessionId === sessionIdRef.current) handleEvent(ev)
    })
      .then((fn) => {
        if (cancelled) {
          fn()
          return
        }
        unsub = fn
        void start("auto")
      })
      .catch(() => {
        if (!cancelled) void start("auto")
      })
    return () => {
      cancelled = true
      activeRef.current = false
      runRef.current += 1
      unsub?.()
      cancelActive()
    }
  }, [agent, locationKey, start, handleEvent, cancelActive])

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!session || submitting) return
    const value = input.trim()
    if (!value) return
    setSubmitting(true)
    setFeedback(null)
    try {
      await ipc.providerLoginSubmit(session.sessionId, value)
    } catch (err) {
      setFeedback(String(err))
      setSubmitting(false)
    }
  }

  const cancel = () => {
    cancelActive()
    onClose()
  }

  const copyUrl = () => {
    if (!session) return
    copyText(session.url)
    setCopiedUrl(true)
    setTimeout(() => setCopiedUrl(false), 1500)
  }

  const copyUserCode = () => {
    if (!session?.userCode) return
    copyText(session.userCode)
    setCopiedCode(true)
    setTimeout(() => setCopiedCode(false), 1500)
  }

  return (
    <div className={cn("flex flex-col gap-2.5 bg-muted/40 px-4 py-3 text-[12px]", className)}>
      <div className="flex items-center gap-2">
        <span className="font-medium text-foreground">Sign in to {AGENT_LABEL[agent]}</span>
        <Button variant="ghost" size="icon-xs" className="ml-auto text-muted-foreground" aria-label="Close sign-in" onClick={cancel}>
          <X className="size-3.5" />
        </Button>
      </div>

      {starting ? (
        <div className="flex items-center gap-2 py-1 text-muted-foreground">
          <LoaderCircle className="size-3.5 animate-spin" />
          <span>Starting sign-in...</span>
        </div>
      ) : error ? (
        <div className="flex flex-col gap-2">
          <p role="alert" className="rounded-md border border-red/30 bg-red/10 px-2.5 py-2 text-[12px] leading-snug text-red">
            {error}
          </p>
          <div className="flex items-center gap-2">
            <Button size="xs" onClick={() => void start(method)}>
              <RefreshCw className="size-3" /> Try again
            </Button>
            <Button variant="outline" size="xs" onClick={cancel}>
              Cancel
            </Button>
          </div>
        </div>
      ) : session ? (
        <>
          {session.kind === "device" && session.userCode ? (
            <div className="flex flex-wrap items-center gap-2 rounded-md border bg-background px-3 py-2">
              <div className="min-w-0 flex-1">
                <div className="text-[11px] text-muted-foreground">One-time code</div>
                <div className="mt-0.5 font-mono text-[16px] font-semibold tracking-widest text-foreground select-all">
                  {session.userCode}
                </div>
              </div>
              <Button variant="outline" size="xs" onClick={copyUserCode}>
                {copiedCode ? <Check className="size-3 text-green" /> : <Copy className="size-3" />}
                {copiedCode ? "Copied" : "Copy code"}
              </Button>
            </div>
          ) : null}

          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="xs"
              variant={session.autoOpened ? "outline" : "default"}
              onClick={() => openSignInUrl(session.url)}
            >
              <ExternalLink className="size-3" /> Open sign-in page
            </Button>
            <Button variant="outline" size="xs" onClick={copyUrl}>
              {copiedUrl ? <Check className="size-3 text-green" /> : <Copy className="size-3" />}
              {copiedUrl ? "Copied URL" : "Copy URL"}
            </Button>
            {session.autoOpened ? (
              <span className="text-[11.5px] text-muted-foreground">Opened in your browser.</span>
            ) : null}
          </div>

          <div className="flex items-center rounded-md border bg-background">
            <code
              className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap px-2.5 py-1.5 font-mono text-[11.5px] text-muted-foreground select-all"
              title={session.url}
            >
              {session.url}
            </code>
          </div>

          {session.hint ? <p className="text-[11.5px] leading-relaxed text-muted-foreground">{session.hint}</p> : null}

          {session.kind === "device" ? (
            <div className="flex flex-wrap items-center gap-2 pt-0.5">
              <span className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">
                <LoaderCircle className="size-3 animate-spin" /> Waiting for sign-in...
              </span>
              <div className="ml-auto flex items-center gap-2">
                {session.canSwitchToBrowser ? (
                  <button
                    type="button"
                    className="text-[11.5px] text-muted-foreground underline underline-offset-2 hover:text-foreground"
                    onClick={() => void start("browser")}
                  >
                    Use browser redirect instead
                  </button>
                ) : null}
                <Button variant="outline" size="xs" onClick={cancel}>
                  Cancel
                </Button>
              </div>
            </div>
          ) : (
            <form onSubmit={submit} className="flex flex-col gap-2">
              {feedback ? (
                <p role="status" className="rounded-md border border-warn/30 bg-warn/10 px-2.5 py-1.5 text-[11.5px] leading-snug text-warn">
                  {feedback}
                </p>
              ) : null}
              <div className="flex flex-wrap items-center gap-2">
                <Input
                  className="h-7 min-w-48 flex-1 font-mono text-[12px]"
                  placeholder={session.inputLabel ?? "Authorization code"}
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  disabled={submitting}
                  autoComplete="off"
                  spellCheck={false}
                />
                <Button type="submit" size="xs" disabled={!input.trim() || submitting}>
                  {submitting ? <LoaderCircle className="size-3 animate-spin" /> : null}
                  Submit
                </Button>
                <Button type="button" variant="outline" size="xs" onClick={cancel}>
                  Cancel
                </Button>
              </div>
              {session.canSwitchToDevice ? (
                <div className="flex items-center">
                  <button
                    type="button"
                    className="text-[11.5px] text-muted-foreground underline underline-offset-2 hover:text-foreground"
                    onClick={() => void start("device")}
                  >
                    Use device code instead
                  </button>
                </div>
              ) : null}
            </form>
          )}
        </>
      ) : null}
    </div>
  )
}
