import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { Check, ChevronDown, ChevronRight, Copy, EyeOff, ExternalLink, LoaderCircle, Plus, RefreshCw, X } from "lucide-react"
import { AgentGlyph } from "@/components/icons"
import { LocationIcon } from "@/components/LocationIcon"
import { useDistros } from "@/components/OpenProject"
import { Button } from "@/components/ui/button"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Tip } from "@/components/Tip"
import { listed, type ListChoice } from "@/lib/provider-list"
import { useChats } from "@/store/chats"
import { useShallow } from "zustand/react/shallow"
import { Input } from "@/components/ui/input"
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import { ipc } from "@/lib/ipc"
import { copyText } from "@/lib/clipboard"
import { PERMISSION_ICON } from "@/components/permission"
import {
  AGENTS,
  AGENT_LABEL,
  PERMISSION_LABEL,
  type Account,
  type AgentKind,
  type Location,
  type PermissionMode,
  type Probe,
  type Provider,
  type Settings,
} from "@/lib/types"
import { useApp } from "@/store/app"
import { useLocationProbes, useProviders } from "@/store/providers"
import { LOCAL_USAGE, usageLocationKey, usageLocationLabel } from "@/store/usage"
import { Dot, Group, Row, Section, Segmented } from "./layout"
import { ProviderLoginPanel } from "./ProviderLoginPanel"

const BLURB: Record<AgentKind, string> = {
  claude: "Anthropic's agent. Uses your Claude subscription or API key from `claude`.",
  codex: "OpenAI's agent. Uses your ChatGPT plan or API key from `codex`.",
  antigravity: "Google's agent over ACP. Uses your Google AI plan from `agy`.",
}

/** Each built-in CLI's docs, and the commands that install it. */
type Installer = { label: string; os: "unix" | "windows" | "any"; command: string }
const INSTALL: Partial<Record<AgentKind, { url: string; options: Installer[] }>> = {
  claude: {
    url: "https://code.claude.com/docs/en/setup",
    options: [
      { label: "macOS / Linux / WSL", os: "unix", command: "curl -fsSL https://claude.ai/install.sh | bash" },
      { label: "Windows", os: "windows", command: "irm https://claude.ai/install.ps1 | iex" },
    ],
  },
  codex: {
    url: "https://learn.chatgpt.com/docs/codex/cli",
    options: [
      { label: "macOS / Linux / WSL", os: "unix", command: "curl -fsSL https://chatgpt.com/codex/install.sh | sh" },
      { label: "Windows", os: "windows", command: 'powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"' },
    ],
  },
  antigravity: {
    url: "https://antigravity.google/docs/getting-started",
    options: [
      { label: "macOS / Linux / WSL", os: "unix", command: "curl -fsSL https://antigravity.google/cli/install.sh | bash" },
      { label: "Windows", os: "windows", command: "irm https://antigravity.google/cli/install.ps1 | iex" },
    ],
  },
}

export function ProvidersSection() {
  const settings = useApp((s) => s.settings)!
  const update = useApp((s) => s.update)
  const settingsFocus = useApp((s) => s.settingsFocus)
  const { distros } = useDistros()
  const [sshConfigHosts, setSshConfigHosts] = useState<string[]>([])

  useEffect(() => {
    let active = true
    void ipc.sshConfigHosts().then(
      (hosts) => { if (active) setSshConfigHosts(hosts) },
      () => {},
    )
    return () => { active = false }
  }, [])

  const availableLocations = useMemo<Location[]>(() => {
    const out: Location[] = [LOCAL_USAGE]
    const seen = new Set<string>(["local"])
    const add = (loc: Location) => {
      const k = loc.kind === "ssh" ? `ssh:${loc.host.toLowerCase()}` : usageLocationKey(loc)
      if (seen.has(k)) return
      seen.add(k)
      out.push(loc)
    }
    for (const h of settings.sshHosts) {
      if (h.host.trim()) add({ kind: "ssh", host: h.host.trim() })
    }
    for (const host of sshConfigHosts) {
      if (host.trim()) add({ kind: "ssh", host: host.trim() })
    }
    for (const p of settings.projects) {
      if (p.location.kind !== "local") add(p.location)
    }
    for (const d of distros ?? []) {
      add({ kind: "wsl", distro: d.name })
    }
    return out
  }, [settings.sshHosts, settings.projects, sshConfigHosts, distros])

  const [selectedKey, setSelectedKey] = useState<string>("local")
  const appliedFocus = useRef<string | null>(null)

  useEffect(() => {
    if (!settingsFocus?.startsWith("location:") || appliedFocus.current === settingsFocus) return
    const wanted = settingsFocus.slice("location:".length)
    if (availableLocations.some((l) => usageLocationKey(l) === wanted)) {
      appliedFocus.current = settingsFocus
      setSelectedKey(wanted)
    }
  }, [settingsFocus, availableLocations])

  const location = useMemo(
    () => availableLocations.find((l) => usageLocationKey(l) === selectedKey) ?? LOCAL_USAGE,
    [availableLocations, selectedKey],
  )
  const isLocal = location.kind === "local"
  const locationKey = usageLocationKey(location)
  const locRef = useRef(location)
  locRef.current = location
  const providersRef = useRef(settings.providers)
  providersRef.current = settings.providers
  const localBinaries = AGENTS.map((a) => settings.providers[a].binary).join("\n")

  const locationProbes = useLocationProbes(location)
  const probes = locationProbes.probes

  useEffect(() => {
    if (isLocal) {
      void useProviders.getState().probeLocal(providersRef.current)
    } else {
      void useProviders.getState().probeRemote(locRef.current, true)
    }
  }, [isLocal, locationKey, localBinaries])

  const used = useChats(useShallow((s) => AGENTS.filter((a) => s.metas.some((m) => m.agent === a))))
  const choices = useProviders((s) => s.choices)
  const setChoice = useProviders((s) => s.setChoice)
  const choose = (kind: AgentKind, choice: ListChoice) => setChoice(kind, choice)
  const shown = (kind: AgentKind) =>
    listed(choices[kind], {
      installed: probes[kind] ? probes[kind]!.found : null,
      isDefault: settings.providers.defaultAgent === kind,
      inUse: used.includes(kind),
    })
  const listedAgents = AGENTS.filter(shown)
  const others = AGENTS.filter((a) => !shown(a))
  const looking = isLocal ? AGENTS.some((a) => !probes[a]) : locationProbes.checkingAll
  const remoteChecking = !isLocal && locationProbes.checkingAll
  const remoteError = !isLocal ? locationProbes.error : null

  return (
    <Section title="Providers" description="Each agent runs and signs in on the computer that hosts the project.">
      <Group>
        <Row label="Location" description="Check CLI status and sign in on this computer, in WSL, or on an SSH host.">
          <Select value={usageLocationKey(location)} onValueChange={setSelectedKey}>
            <SelectTrigger size="sm" className="w-56" aria-label="Provider location">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {availableLocations.map((loc) => {
                const k = usageLocationKey(loc)
                return (
                  <SelectItem key={k} value={k}>
                    <span className="flex items-center gap-2">
                      <LocationIcon location={loc} className="size-3.5" />
                      <span>{usageLocationLabel(loc)}</span>
                    </span>
                  </SelectItem>
                )
              })}
            </SelectContent>
          </Select>
        </Row>
        <Row label="Default agent" description="New chats use this agent unless the project has its own default.">
          <Select
            value={settings.providers.defaultAgent}
            onValueChange={(v) => update((s) => void (s.providers.defaultAgent = v as Settings["providers"]["defaultAgent"]))}
          >
            <SelectTrigger size="sm" className="w-48" aria-label="Default agent">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {AGENTS.filter((a) => settings.providers[a].enabled).map((a) => (
                <SelectItem key={a} value={a}>
                  {AGENT_LABEL[a]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Row>
      </Group>

      {remoteChecking ? (
        <Group>
          <div className="flex items-center gap-2 px-4 py-3 text-[12.5px] text-muted-foreground">
            <LoaderCircle className="size-3.5 animate-spin" />
            <span>Checking {usageLocationLabel(location)}...</span>
          </div>
        </Group>
      ) : null}

      {remoteError ? (
        <Group>
          <div className="flex items-center gap-3 px-4 py-3 text-[12.5px]">
            <Dot tone="err" />
            <span className="min-w-0 flex-1 text-red">{remoteError}</span>
            <Button variant="outline" size="xs" onClick={() => void useProviders.getState().probeRemote(location, true)}>
              <RefreshCw className="size-3" /> Retry
            </Button>
          </div>
        </Group>
      ) : null}

      {listedAgents.map((a) => (
        <ProviderCard
          key={a}
          kind={a}
          location={location}
          provider={settings.providers[a]}
          probe={probes[a] ?? null}
          checking={isLocal ? !!locationProbes.checking[a] : locationProbes.checkingAll}
          remoteError={remoteError}
          onCheck={() => void (isLocal ? useProviders.getState().probeLocalAgent(a, settings.providers[a].binary) : useProviders.getState().probeRemote(location, true))}
          onHide={() => choose(a, "hidden")}
        />
      ))}
      {others.length ? (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="outline" className="self-start border-dashed text-muted-foreground hover:text-foreground">
              <Plus /> Add provider <ChevronDown className="opacity-60" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-72">
            <DropdownMenuLabel className="text-[11px] font-normal text-muted-foreground">
              {looking ? "Looking for installed agents..." : "Installed agents show up here by themselves."}
            </DropdownMenuLabel>
            {others.map((a) => (
              <DropdownMenuItem key={a} onSelect={() => choose(a, "shown")}>
                <AgentGlyph agent={a} className="size-4" />
                <span className="flex-1">{AGENT_LABEL[a]}</span>
                <span className="text-[11px] text-faint">{!probes[a] ? "Checking..." : probes[a]!.found ? "Hidden" : "Not installed"}</span>
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      ) : null}
    </Section>
  )
}

function ProviderCard({
  kind,
  location,
  provider,
  probe,
  checking,
  remoteError,
  onCheck,
  onHide,
}: {
  kind: AgentKind
  location: Location
  provider: Provider
  probe: Probe | null
  checking: boolean
  remoteError: string | null
  onCheck: () => void
  onHide: () => void
}) {
  const update = useApp((s) => s.update)
  const edit = (f: (p: Provider) => void) => update((s) => f(s.providers[kind]))

  const status = remoteError ? (
    <>
      <Dot tone="err" /> Unreachable
    </>
  ) : !probe ? (
    <span className="text-faint">Checking...</span>
  ) : probe.found && probe.version ? (
    <>
      <Dot tone={probe.outdated ? "warn" : "ok"} /> v{probe.version}
      {probe.outdated ? <span className="text-warn">· too old</span> : null}
    </>
  ) : probe.found ? (
    <>
      <Dot tone="warn" /> {probe.error ?? "Installed"}
    </>
  ) : (
    <>
      <Dot tone="err" /> Not installed
    </>
  )

  return (
    <Group>
      <div className="flex items-center gap-3.5 px-4 py-3">
        <AgentGlyph agent={kind} className="size-6 shrink-0" />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2 text-[14px] font-semibold">
            {AGENT_LABEL[kind]}
            <span className="flex items-center gap-1.5 text-[11.5px] font-normal text-muted-foreground">{status}</span>
          </div>
          <div className="mt-0.5 truncate text-[12px] text-muted-foreground">{probe?.path ?? BLURB[kind]}</div>
        </div>
        {INSTALL[kind] ? (
          <Button variant="ghost" size="xs" className="text-muted-foreground" onClick={() => import("@tauri-apps/plugin-opener").then((o) => o.openUrl(INSTALL[kind]!.url))}>
            Docs <ExternalLink className="size-3" />
          </Button>
        ) : null}
        <Button variant="ghost" size="icon-sm" aria-label="Check again" onClick={onCheck} disabled={checking}>
          <RefreshCw className={checking ? "animate-spin" : undefined} />
        </Button>
        <Tip content="Hide this provider. Use Add provider to show it again.">
          <Button variant="ghost" size="icon-sm" aria-label={`Hide ${AGENT_LABEL[kind]}`} onClick={onHide}>
            <EyeOff />
          </Button>
        </Tip>
        <Switch checked={provider.enabled} onCheckedChange={(v) => edit((p) => void (p.enabled = v))} aria-label={`Use ${AGENT_LABEL[kind]}`} />
      </div>
      {probe?.outdated ? <div className="border-t border-warn/30 bg-warn/10 px-4 py-2.5 text-[12px] text-warn">{probe.outdated}</div> : null}
      {!remoteError && probe && !probe.found && INSTALL[kind] ? <InstallHelp kind={kind} remote={location.kind !== "local"} /> : null}
      <SignIn kind={kind} location={location} installed={!remoteError && !!probe?.found} />
      <details className="group/advanced">
        <summary className="flex min-h-12 cursor-pointer list-none items-center gap-2 px-4 py-2.5 text-[13px] font-medium text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
          <ChevronRight className="size-4 shrink-0 transition-transform group-open/advanced:rotate-90" />
          Advanced settings
        </summary>
        <div className="divide-y border-t">
          <Row label="Default model" description="Leave blank to use the agent's default model.">
            <Input
              className="h-7 w-56 font-mono text-[12px]"
              placeholder={kind === "claude" ? "opus, sonnet" : kind === "codex" ? "gpt-5.5-codex" : "gemini-3.8-flash"}
              defaultValue={provider.defaultModel}
              onBlur={(e) => edit((p) => void (p.defaultModel = e.target.value.trim()))}
            />
          </Row>
          <Row label="Permissions" description="What the agent may do without asking.">
            <Segmented<PermissionMode>
              value={provider.permissionMode}
              options={(["ask", "acceptEdits", "plan", "fullAccess"] as const).map((m) => {
                const Icon = PERMISSION_ICON[m]
                return { value: m, label: <span className="flex items-center gap-1.5"><Icon className="size-3.5" />{PERMISSION_LABEL[m]}</span> }
              })}
              onChange={(v) => edit((p) => void (p.permissionMode = v))}
            />
          </Row>
          <Row label="Executable" description="Leave blank to find the executable on PATH.">
            <Input
              className="h-7 w-48 font-mono text-[12px]"
              placeholder={kind}
              defaultValue={provider.binary}
              onBlur={(e) => edit((p) => void (p.binary = e.target.value.trim()))}
            />
          </Row>
          <EnvRows kind={kind} label={AGENT_LABEL[kind]} provider={provider} />
        </div>
      </details>
    </Group>
  )
}

/** How to install a missing CLI: the command for this OS, copyable. */
function InstallHelp({ kind, remote }: { kind: AgentKind; remote: boolean }) {
  const { options } = INSTALL[kind]!
  const platform = useApp((s) => s.info?.platform)
  const os = remote ? "unix" : platform === "windows" ? "windows" : "unix"
  const [label, setLabel] = useState(() => (options.find((o) => o.os === os) ?? options.find((o) => o.os === "any") ?? options[0]).label)
  const [copied, setCopied] = useState(false)
  const command = (options.find((o) => o.label === label) ?? options[0]).command
  const copy = () => {
    copyText(command)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }
  return (
    <div className="flex flex-col gap-2 bg-muted/40 px-4 py-3">
      <span className="text-[13px] font-medium">Install {AGENT_LABEL[kind]}</span>
      {options.length > 1 ? (
        <div className="self-start">
          <Segmented value={label} options={options.map((o) => ({ value: o.label, label: o.label }))} onChange={setLabel} />
        </div>
      ) : null}
      <div className="flex items-center rounded-md border bg-background">
        <code className="min-w-0 flex-1 truncate px-2.5 py-1.5 font-mono text-[12px] select-all" title={command}>
          {command}
        </code>
        <Button variant="ghost" size="icon-sm" className="mr-0.5" aria-label={copied ? "Copied" : "Copy install command"} onClick={copy}>
          {copied ? <Check className="text-green" /> : <Copy />}
        </Button>
      </div>
      <p className="text-[11.5px] text-faint">Run it in a terminal, then switch back here to re-check. Still not found? Restart Moonfish.</p>
    </div>
  )
}

function SignIn({ kind, location, installed }: { kind: AgentKind; location: Location; installed: boolean }) {
  const [account, setAccount] = useState<Account | null>(null)
  const [signingIn, setSigningIn] = useState(false)
  const locationKey = usageLocationKey(location)
  const locRef = useRef(location)
  locRef.current = location
  const reqRef = useRef(0)

  const check = useCallback(() => {
    const req = ++reqRef.current
    setAccount(null)
    ipc.providerAccount(kind, locRef.current).then(
      (acc) => { if (req === reqRef.current) setAccount(acc) },
      (e) => { if (req === reqRef.current) setAccount({ signedIn: false, label: null, plan: null, error: String(e) }) },
    )
  }, [kind, locationKey])

  useEffect(() => {
    setSigningIn(false)
    if (installed) {
      check()
    } else {
      reqRef.current += 1
      setAccount(null)
    }
    return () => { reqRef.current += 1 }
  }, [installed, locationKey, check])

  const reloadCatalog = useCallback(() => {
    const projects = useApp.getState().settings?.projects ?? []
    const match = projects.find((p) => usageLocationKey(p.location) === locationKey)
    if (match || location.kind === "local") {
      void ipc.providerCatalog(kind, match?.id ?? null).catch(() => {})
    }
  }, [kind, locationKey, location.kind])

  const desc = !installed ? (
    location.kind === "local" ? "Install it first." : `Install it on ${usageLocationLabel(location)} first.`
  ) : !account ? (
    "Checking..."
  ) : account.signedIn ? (
    <span className="flex flex-wrap items-center gap-1.5">
      <Dot tone="ok" /> Signed in{account.label ? <> as <span className="text-foreground">{account.label}</span></> : null}
      {account.plan ? <span className="rounded-full border px-1.5 font-mono text-[10.5px] uppercase">{account.plan}</span> : null}
    </span>
  ) : (
    <span className="flex items-center gap-1.5">
      <Dot tone="warn" /> {account.error ?? "Not signed in"}
    </span>
  )

  return (
    <>
      <Row label="Sign-in" description={desc}>
        {installed && account && !signingIn ? (
          account.signedIn ? (
            <Button variant="outline" size="xs" onClick={() => setSigningIn(true)}>
              Switch account
            </Button>
          ) : (
            <Button size="xs" onClick={() => setSigningIn(true)}>
              Sign in
            </Button>
          )
        ) : null}
        <Button variant="outline" size="xs" onClick={check} disabled={!installed}>
          Check
        </Button>
      </Row>
      {signingIn && installed ? (
        <ProviderLoginPanel
          key={`${kind}:${locationKey}`}
          agent={kind}
          location={location}
          onDone={(acc) => {
            setAccount(acc)
            reloadCatalog()
          }}
          onClose={() => setSigningIn(false)}
        />
      ) : null}
    </>
  )
}

function EnvRows({ kind, label, provider }: { kind: AgentKind; label: string; provider: Provider }) {
  const update = useApp((s) => s.update)
  const edit = (f: (p: Provider) => void) => update((s) => f(s.providers[kind]))
  return (
    <div className="px-4 py-2.5">
      <div className="flex items-center">
        <div className="flex-1">
          <div className="text-[13px] font-medium">Environment</div>
          <div className="mt-0.5 text-[12px] text-muted-foreground">Extra variables for the {label} process. Moonfish stores values in your OS keychain, not in the settings file.</div>
        </div>
        <Button variant="outline" size="xs" onClick={() => edit((p) => void p.env.push({ key: "", value: "", inKeychain: false }))}>
          <Plus /> Add
        </Button>
      </div>
      {provider.env.length ? (
        <div className="mt-2 flex flex-col gap-1">
          {provider.env.map((v, i) => (
            <div key={i} className="flex gap-1">
              <Input className="h-7 w-48 font-mono text-[12px]" placeholder="NAME" defaultValue={v.key} onBlur={(e) => edit((p) => void (p.env[i].key = e.target.value.trim()))} />
              <Input
                className="h-7 flex-1 font-mono text-[12px]"
                placeholder={v.inKeychain ? "Saved in keychain. Type to replace." : "value"}
                type="password"
                defaultValue={v.value}
                onBlur={(e) => e.target.value && edit((p) => void (p.env[i].value = e.target.value))}
              />
              <Button variant="ghost" size="icon-sm" aria-label="Remove" onClick={() => edit((p) => void p.env.splice(i, 1))}>
                <X />
              </Button>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  )
}
