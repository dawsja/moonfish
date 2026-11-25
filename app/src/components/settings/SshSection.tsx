import { useEffect, useRef, useState } from "react"
import { ChevronDown, CodeXml, FolderPlus, LoaderCircle, Pencil, Plus, RefreshCw, Server, Trash2 } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { blankSshHost, editableSshHosts, removeSshHost, writeSshHost, type SshConfigEntry, type SshConfigFields } from "@/lib/ssh-config"
import { toast } from "sonner"
import { ipc } from "@/lib/ipc"
import { AGENTS, type Account, type AgentKind, type HostCheck, type Tool } from "@/lib/types"
import { cn } from "@/lib/utils"
import { useApp } from "@/store/app"
import { useProviders } from "@/store/providers"
import { Dot, Group, Section } from "./layout"
import { ProviderLoginPanel } from "./ProviderLoginPanel"
import { pickRemoteFolder } from "./RemoteFolder"

type HostDialog = { kind: "add" } | { kind: "edit"; entry: SshConfigEntry } | { kind: "delete"; host: string; entry?: SshConfigEntry }

const AGENT_TOOLS: Partial<Record<string, AgentKind>> = { claude: "claude", codex: "codex", antigravity: "antigravity" }

export function SshSection() {
  const shortcuts = useApp((s) => s.settings!.sshHosts)
  const update = useApp((s) => s.update)
  const addProject = useApp((s) => s.addProject)
  const [configHosts, setConfigHosts] = useState<string[]>([])
  const [config, setConfig] = useState<string | null | undefined>(undefined)
  const [loadError, setLoadError] = useState("")
  const [loading, setLoading] = useState(false)
  const [draft, setDraft] = useState("")
  const [checks, setChecks] = useState<Record<string, HostCheck | "checking">>({})
  const [accounts, setAccounts] = useState<Record<string, Partial<Record<AgentKind, Account | "checking">>>>({})
  const [loginTarget, setLoginTarget] = useState<{ host: string; agent: AgentKind } | null>(null)
  const [dialog, setDialog] = useState<HostDialog | null>(null)
  const [advanced, setAdvanced] = useState(false)
  const configRevision = useRef(0)

  const reload = async () => {
    const revision = ++configRevision.current
    setLoading(true)
    setLoadError("")
    setChecks({})
    setAccounts({})
    setLoginTarget(null)
    const [file, aliases] = await Promise.allSettled([ipc.sshConfigRead(), ipc.sshConfigHosts()])
    if (revision !== configRevision.current) return
    setConfig(file.status === "fulfilled" ? file.value : undefined)
    if (file.status === "rejected") setLoadError(String(file.reason))
    if (aliases.status === "fulfilled") setConfigHosts(aliases.value)
    setLoading(false)
  }

  useEffect(() => {
    void reload()
    return () => { configRevision.current += 1 }
  }, [])

  const configSaved = (file: string, aliases: string[]) => {
    configRevision.current += 1
    setLoading(false)
    setConfig(file)
    setConfigHosts(aliases)
    setChecks({})
    setAccounts({})
    setLoginTarget(null)
  }

  const test = async (host: string) => {
    const revision = configRevision.current
    const location = { kind: "ssh" as const, host }
    setLoginTarget((cur) => (cur?.host === host ? null : cur))
    setChecks((c) => ({ ...c, [host]: "checking" }))
    setAccounts((a) => ({ ...a, [host]: {} }))
    const r = await ipc.hostCheck(location).catch((e) => ({ ok: false, error: String(e) }) as HostCheck)
    useProviders.getState().recordHostCheck(location, r)
    if (revision !== configRevision.current) return
    setChecks((c) => ({ ...c, [host]: r }))
    if (!r.ok) return
    for (const kind of AGENTS) {
      if (!r[kind].path) continue
      setAccounts((prev) => ({ ...prev, [host]: { ...prev[host], [kind]: "checking" } }))
      ipc.providerAccount(kind, location).then(
        (acc) => {
          if (revision === configRevision.current) {
            setAccounts((prev) => ({ ...prev, [host]: { ...prev[host], [kind]: acc } }))
          }
        },
        (e) => {
          if (revision === configRevision.current) {
            setAccounts((prev) => ({
              ...prev,
              [host]: { ...prev[host], [kind]: { signedIn: false, label: null, plan: null, error: String(e) } },
            }))
          }
        },
      )
    }
  }

  const addFolder = async (host: string) => {
    const path = await pickRemoteFolder({ kind: "ssh", host })
    if (path && await addProject({ kind: "ssh", host }, path)) useApp.getState().closeSettings()
  }

  const entries = editableSshHosts(config ?? "")
  const known = new Map<string, { host: string; label?: string; entry?: SshConfigEntry; shortcut?: boolean }>()
  for (const host of configHosts) known.set(host.toLowerCase(), { host })
  for (const entry of entries) known.set(entry.values.alias.toLowerCase(), { host: entry.values.alias, entry })
  for (const h of shortcuts) {
    const key = h.host.toLowerCase()
    const row = known.get(key)
    if (row) row.label = h.label
    else known.set(key, { host: h.host, label: h.label, shortcut: true })
  }

  const save = async (values?: SshConfigFields) => {
    if (!dialog) return
    if (dialog.kind === "delete" && !dialog.entry) {
      await update((s) => { s.sshHosts = s.sshHosts.filter((h) => h.host !== dialog.host) })
      return
    }
    if (config === undefined) throw new Error("Reload the SSH config before saving.")
    if (dialog.kind === "add" && values && known.has(values.alias.trim().toLowerCase())) throw new Error("That SSH alias already exists.")
    let next: string
    if (dialog.kind === "delete") {
      if (!dialog.entry) return
      next = removeSshHost(config ?? "", dialog.entry)
    } else {
      if (!values) return
      next = writeSshHost(config ?? "", values, dialog.kind === "edit" ? dialog.entry : undefined)
    }
    if (config !== next) configSaved(next, await ipc.sshConfigSave(config, next))
    toast.success(dialog.kind === "delete" ? "SSH host deleted" : "SSH host saved")
  }

  const addShortcut = (e: React.FormEvent) => {
    e.preventDefault()
    const host = draft.trim()
    if (!host || known.has(host.toLowerCase()) || /^-|[\s\x00-\x1f\x7f]/.test(host)) return
    void update((s) => { s.sshHosts.push({ host, port: 0, identityFile: "", label: "" }) })
    setDraft("")
  }

  return (
    <Section
      title="SSH hosts"
      description="Connect to remote machines with an SSH key, then add their folders as projects."
      actions={<Button size="sm" onClick={() => setDialog({ kind: "add" })} disabled={loading || config === undefined}><Plus /> Add host</Button>}
    >
      {loadError ? <p className="text-[12px] text-red" role="alert">{loadError}</p> : null}
      <Group title="Hosts" aside={
        <Button variant="ghost" size="xs" onClick={reload} disabled={loading}>
          <RefreshCw className={loading ? "animate-spin" : undefined} /> Refresh
        </Button>
      }>
        {known.size === 0 ? (
          <div className="px-4 py-8 text-center">
            <Server className="mx-auto mb-2 size-5 text-muted-foreground" />
            <div className="text-[13px] font-medium">{loading ? "Loading hosts..." : "No SSH hosts yet"}</div>
            {!loading ? <p className="mt-1 text-[12px] text-muted-foreground">Add a host to save its connection details in your SSH config.</p> : null}
          </div>
        ) : Array.from(known.values()).map((h) => {
          const c = checks[h.host]
          const hostAccounts = accounts[h.host] ?? {}
          const entry = h.entry
          const info = entry?.values
          const activeLogin = loginTarget?.host === h.host ? loginTarget.agent : null
          return (
            <div key={h.host}>
              <div className="flex flex-wrap items-center gap-x-3 gap-y-2 px-4 py-3 sm:flex-nowrap">
                <span className="grid size-9 shrink-0 place-items-center rounded-md border bg-background"><Server className="size-4 text-cyan" /></span>
                <div className="min-w-0 flex-1">
                  <div className="truncate font-mono text-[13px] font-medium">{h.label || h.host}</div>
                  <div className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11.5px] text-muted-foreground">
                    {c === "checking" ? <><LoaderCircle className="size-3 animate-spin" /> Connecting...</>
                      : c?.ok ? <><Dot tone="ok" /> Connected · {c.os} · {c.home}</>
                        : c ? <><Dot tone="err" /> <span className="truncate text-red" title={c.error ?? undefined}>{c.error}</span></>
                          : <span className="truncate">{info?.hostname
                            ? `${info.user ? info.user + "@" : ""}${info.hostname}${info.port ? " · port " + info.port : ""}`
                            : h.shortcut ? "Moonfish shortcut" : h.entry ? "SSH config" : "Included or shared SSH config entry"}</span>}
                  </div>
                </div>
                <div className="flex shrink-0 items-center gap-1.5">
                  <Button variant="outline" size="xs" onClick={() => test(h.host)} disabled={c === "checking"}>Test</Button>
                  <Button variant="outline" size="xs" onClick={() => addFolder(h.host)}><FolderPlus /> Add folder</Button>
                  {entry ? <Button variant="ghost" size="icon-xs" title="Edit host" aria-label={`Edit ${h.host}`} disabled={loading} onClick={() => setDialog({ kind: "edit", entry })}><Pencil /></Button> : null}
                  {h.entry || h.shortcut ? (
                    <Button variant="ghost" size="icon-xs" title="Delete host" aria-label={`Delete ${h.host}`} disabled={loading} onClick={() => setDialog({ kind: "delete", host: h.host, entry: h.entry })}>
                      <Trash2 />
                    </Button>
                  ) : null}
                </div>
              </div>
              {c && c !== "checking" && c.ok ? (
                <>
                  <ToolGrid
                    check={c}
                    extra={(t) => {
                      const kind = AGENT_TOOLS[t]
                      if (!kind || !c[t].path) return null
                      const a = hostAccounts[kind]
                      if (!a) return null
                      if (a === "checking") return <div className="mt-1 text-[10.5px] text-faint">Checking sign-in...</div>
                      if (a.signedIn) {
                        return (
                          <div className="mt-1 truncate text-[10.5px] text-green" title={a.plan ? `${a.label ?? "Signed in"} (${a.plan})` : (a.label ?? "Signed in")}>
                            {a.label ?? "Signed in"}
                          </div>
                        )
                      }
                      return (
                        <button
                          type="button"
                          className="mt-1 text-[10.5px] text-foreground underline underline-offset-2"
                          onClick={() => setLoginTarget({ host: h.host, agent: kind })}
                        >
                          Sign in
                        </button>
                      )
                    }}
                  />
                  {activeLogin ? (
                    <ProviderLoginPanel
                      key={`${activeLogin}:ssh:${h.host}`}
                      className="border-t"
                      agent={activeLogin}
                      location={{ kind: "ssh", host: h.host }}
                      onDone={(acc) =>
                        setAccounts((prev) => ({
                          ...prev,
                          [h.host]: { ...prev[h.host], [activeLogin]: acc },
                        }))
                      }
                      onClose={() => setLoginTarget(null)}
                    />
                  ) : null}
                </>
              ) : null}
            </div>
          )
        })}
      </Group>
      <Group>
        <button type="button" className="flex w-full items-center gap-2 p-3 text-left text-[12px] font-medium hover:bg-accent" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
          <CodeXml className="size-4 text-muted-foreground" /> Advanced SSH settings
          <ChevronDown className={cn("ml-auto size-4 text-muted-foreground", advanced && "rotate-180")} />
        </button>
        {advanced ? (
          <div className="space-y-4 p-3">
            <form onSubmit={addShortcut} className="flex flex-wrap items-end gap-2">
              <label className="min-w-48 flex-1 space-y-1.5 text-[12px]">Save a shortcut in Moonfish
                <Input className="h-8 font-mono text-[12px]" placeholder="user@hostname" value={draft} onChange={(e) => setDraft(e.target.value)} />
              </label>
              <Button variant="outline" type="submit" size="sm" disabled={!draft.trim() || known.has(draft.trim().toLowerCase())}>Add shortcut</Button>
            </form>
            <SshConfigEditor onSaved={configSaved} />
          </div>
        ) : null}
      </Group>
      {dialog ? <HostEditor dialog={dialog} onSave={save} onClose={() => setDialog(null)} /> : null}
    </Section>
  )
}

function HostEditor({ dialog, onSave, onClose }: { dialog: HostDialog; onSave: (fields?: SshConfigFields) => Promise<void>; onClose: () => void }) {
  const [values, setValues] = useState(dialog.kind === "edit" ? dialog.entry.values : blankSshHost)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const isDelete = dialog.kind === "delete"
  const chooseIdentityFile = async () => {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog")
      const path = await open({ title: "Choose an SSH private key" })
      if (typeof path === "string") setValues((old) => ({ ...old, identityFile: path }))
    } catch (e) {
      setError(String(e))
    }
  }
  const field = (key: keyof SshConfigFields, label: string, placeholder: string) => (
    <div className={cn("min-w-0 space-y-1.5 text-[12px] font-medium", key === "identityFile" && "sm:col-span-2")}>
      <label htmlFor={`ssh-host-${key}`}>{label}</label>
      <div className="flex gap-2">
        <Input
          id={`ssh-host-${key}`}
          className="font-mono text-[12px] read-only:text-muted-foreground"
          value={values[key]}
          onChange={(e) => setValues((old) => ({ ...old, [key]: e.target.value }))}
          readOnly={key === "alias" && dialog.kind === "edit"}
          tabIndex={key === "alias" && dialog.kind === "edit" ? -1 : undefined}
          required={dialog.kind === "add" && (key === "alias" || key === "hostname")}
          placeholder={placeholder}
          autoComplete="off"
          spellCheck={false}
          inputMode={key === "port" ? "numeric" : undefined}
        />
        {key === "identityFile" ? <Button type="button" variant="outline" onClick={chooseIdentityFile}>Browse...</Button> : null}
      </div>
    </div>
  )
  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (busy) return
    setBusy(true)
    setError("")
    try {
      await onSave(isDelete ? undefined : values)
      onClose()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      setBusy(false)
    }
  }
  return (
    <Dialog open onOpenChange={(open) => { if (!open && !busy) onClose() }}>
      <DialogContent showCloseButton={!busy}>
        <DialogHeader>
          <DialogTitle>{isDelete ? `Delete ${dialog.host}?` : dialog.kind === "edit" ? `Edit ${dialog.entry.values.alias}` : "Add SSH host"}</DialogTitle>
          <DialogDescription>{isDelete
            ? dialog.entry ? "Moonfish will delete the host block from ~/.ssh/config. Projects that use this alias may lose access." : "This removes the shortcut from Moonfish."
            : "Moonfish saves the host in ~/.ssh/config. SSH uses its defaults for blank optional fields."}</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="space-y-4">
          {!isDelete ? (
            <fieldset disabled={busy} className="grid gap-3 sm:grid-cols-2">
              {field("alias", "Alias", "devbox")}
              {field("hostname", "Hostname or IP", "devbox.example.com")}
              {field("user", "SSH user", "Current user")}
              {field("port", "Port", "22")}
              {field("identityFile", "Identity file", "~/.ssh/id_ed25519")}
            </fieldset>
          ) : null}
          {error ? <p role="alert" className="text-[12px] text-red">{error}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>Cancel</Button>
            <Button type="submit" variant={isDelete ? "destructive" : "default"} disabled={busy}>
              {busy ? <LoaderCircle className="animate-spin" /> : null}
              {isDelete ? "Delete host" : dialog.kind === "edit" ? "Save changes" : "Add host"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function SshConfigEditor({ onSaved }: { onSaved: (config: string, hosts: string[]) => void }) {
  const [original, setOriginal] = useState<string | null | undefined>(undefined)
  const [text, setText] = useState("")
  const [busy, setBusy] = useState(false)
  const [editing, setEditing] = useState(false)
  const [error, setError] = useState("")
  const [saved, setSaved] = useState(false)
  const dirty = original !== undefined && text !== (original ?? "").replace(/\r\n?/g, "\n")

  const load = async () => {
    setEditing(true)
    setBusy(true)
    setError("")
    setSaved(false)
    try {
      const config = await ipc.sshConfigRead()
      setOriginal(config)
      setText((config ?? "").replace(/\r\n?/g, "\n"))
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  const save = async () => {
    if (original === undefined || !dirty || busy) return
    setBusy(true)
    setError("")
    setSaved(false)
    const config = original?.includes("\r\n") ? text.replace(/\n/g, "\r\n") : text
    try {
      const hosts = await ipc.sshConfigSave(original, config)
      setOriginal(config)
      onSaved(config, hosts)
      setSaved(true)
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Group title="SSH config">
      <div className="space-y-3 p-3">
        <div className="flex items-center justify-between gap-3">
          <p className="text-[12px] text-muted-foreground">
            Edit ~/.ssh/config to change shared host blocks, patterns, or other directives.
          </p>
          {!editing ? <Button variant="outline" size="sm" onClick={load}>Edit raw config</Button> : null}
        </div>
        {editing ? (
          <>
            {original !== undefined ? (
              <textarea
                aria-label="SSH config"
                className="min-h-48 w-full resize-y rounded-md border border-input bg-background p-3 font-mono text-[12px] leading-relaxed focus-visible:outline-2 focus-visible:outline-ring"
                disabled={busy}
                spellCheck={false}
                autoCapitalize="off"
                autoComplete="off"
                wrap="off"
                value={text}
                onChange={(e) => {
                  setText(e.target.value)
                  setSaved(false)
                }}
              />
            ) : null}
            {error ? <p role="alert" className="text-[12px] text-red">{error}</p> : null}
            <div className="flex flex-wrap items-center gap-2">
              <Button size="sm" onClick={save} disabled={busy || !dirty}>Save config</Button>
              <Button variant="outline" size="sm" onClick={load} disabled={busy}>
                {dirty ? "Discard edits and reload" : "Reload"}
              </Button>
              <span role="status" className="text-[12px] text-muted-foreground">
                {busy ? "Updating SSH config" : saved ? "Config saved." : original === null ? "Moonfish will create ~/.ssh/config when you save." : null}
              </span>
            </div>
          </>
        ) : null}
      </div>
    </Group>
  )
}

const TOOLS = [
  ["claude", "Claude Code"],
  ["codex", "Codex"],
  ["antigravity", "Antigravity"],
  ["git", "git"],
  ["gh", "gh"],
] as const

/** What a host or distro has installed. Agents are optional (any one will do); git is needed. */
export function ToolGrid({ check, extra }: { check: HostCheck; extra?: (tool: (typeof TOOLS)[number][0]) => React.ReactNode }) {
  return (
    <div className="grid grid-cols-3 gap-px sm:grid-cols-5 border-t bg-border">
      {TOOLS.map(([t, name]) => (
        <ToolCell key={t} name={name} tool={check[t]} needed={t === "git"}>
          {extra?.(t)}
        </ToolCell>
      ))}
    </div>
  )
}

function ToolCell({ name, tool, needed, children }: { name: string; tool: Tool; needed: boolean; children?: React.ReactNode }) {
  const version = tool.version?.match(/\d+\.\d+(\.\d+)?/)?.[0]
  return (
    <div className="bg-card px-3 py-2" title={tool.windows ? `Only a Windows copy at ${tool.windows} is on this distribution's PATH. Install the Linux version there.` : (tool.path ?? undefined)}>
      <div className="flex items-center gap-1.5 text-[12px]">
        <Dot tone={tool.path ? "ok" : needed || tool.windows ? "warn" : "off"} /> {name}
      </div>
      <div className={cn("mt-0.5 truncate font-mono text-[11px]", tool.path ? "text-muted-foreground" : tool.windows ? "text-warn" : "text-faint")}>
        {tool.path ? (version ? `v${version}` : "installed") : tool.windows ? "Windows copy only" : "not installed"}
      </div>
      {children}
    </div>
  )
}
