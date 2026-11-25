import { useEffect, useState } from "react"
import { CircleCheck, Smartphone } from "lucide-react"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { confirmAction } from "@/components/Prompt"
import { remote, seenLabel, useRemote, type PairingInfo } from "@/lib/remote"
import { Dot, Group, Row, Section } from "./layout"

/** The address and the code to type on the phone, until a phone pairs or the code runs out. */
function PairDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (o: boolean) => void }) {
  const [info, setInfo] = useState<PairingInfo | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [now, setNow] = useState(Date.now())
  const paired = useRemote((s) => s.paired)
  const failedLeft = useRemote((s) => s.failedLeft)

  const fresh = () => {
    setError(null)
    useRemote.setState({ paired: null, failedLeft: null })
    remote.pairStart().then(setInfo, (e) => setError(String(e)))
  }
  useEffect(() => {
    if (!open) return
    fresh()
    return () => void remote.pairCancel().catch(() => {})
  }, [open])
  useEffect(() => {
    if (!open) return
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [open])
  // A wrong code five times closes it on the desktop too: the code is shown as used up, with a
  // way to make a new one (`info` stays, so the dialog doesn't go back to "Making a code…").
  const left = info && failedLeft !== 0 ? Math.max(0, Math.round((info.expiresAt - now) / 1000)) : 0
  const expired = !!info && left === 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Pair a phone</DialogTitle>
          <DialogDescription>Enter this address and code in Moonfish on your phone.</DialogDescription>
        </DialogHeader>
        {paired ? (
          <div className="flex flex-col items-center gap-3 py-6 text-center">
            <CircleCheck className="size-10 text-green" />
            <div className="text-[14px] font-medium">Paired with {paired}</div>
            <p className="text-[12px] text-muted-foreground">It can open your projects' chats now, whenever this computer is on and reachable.</p>
            <Button size="sm" onClick={() => onOpenChange(false)}>
              Done
            </Button>
          </div>
        ) : error ? (
          <div className="rounded-lg border border-red/40 bg-red/5 px-3 py-2.5 text-[12.5px] text-red">{error}</div>
        ) : info && !expired ? (
          // The same order as the phone's form: address, then code.
          <div className="flex flex-col gap-3">
            <div className="rounded-lg border bg-card px-4 py-3">
              <div className="mb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">Address</div>
              <div className="font-mono text-[20px] font-semibold" aria-label="Address">
                {info.addresses[0]}
              </div>
              {info.addresses.length > 1 ? (
                <div className="mt-1 text-[11.5px] text-muted-foreground">
                  Or, on another network: <span className="font-mono">{info.addresses.slice(1).join(", ")}</span>
                </div>
              ) : null}
            </div>
            <div className="rounded-lg border bg-card px-4 py-3">
              <div className="mb-1 text-[11px] font-medium tracking-wide text-muted-foreground uppercase">Code</div>
              <div className="font-mono text-[30px] font-semibold tracking-[0.18em]" aria-label="Pairing code">
                {info.code}
              </div>
              <div className="mt-1 text-[11.5px] text-muted-foreground">
                Works once, for {Math.floor(left / 60)}:{String(left % 60).padStart(2, "0")}
                {failedLeft !== null && failedLeft > 0 ? <span className="text-warn"> · wrong code tried, {failedLeft} tries left</span> : null}
              </div>
            </div>
          </div>
        ) : (
          <div className="flex flex-col items-center gap-3 py-6 text-center">
            <p className="text-[12.5px] text-muted-foreground">{info ? (failedLeft === 0 ? "Too many wrong attempts for this code." : "That code expired.") : "Making a code…"}</p>
            {info ? (
              <Button size="sm" onClick={fresh}>
                New code
              </Button>
            ) : null}
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}

export function PhoneSection() {
  const state = useRemote((s) => s.state)
  const setPairing = (pairingOpen: boolean) => useRemote.setState({ pairingOpen })
  const [port, setPort] = useState("")
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    void useRemote.getState().refresh()
  }, [])
  useEffect(() => {
    if (state) setPort(String(state.port))
  }, [state?.port])

  const toggle = async (on: boolean) => {
    setBusy(true)
    await remote.setEnabled(on).then((s) => useRemote.setState({ state: s }), (e) => toast.error(String(e)))
    await useRemote.getState().refresh()
    setBusy(false)
  }
  const savePort = async () => {
    const n = Number(port)
    if (!state || n === state.port) return
    await remote.setPort(n).then((s) => useRemote.setState({ state: s }), (e) => (toast.error(String(e)), setPort(String(state.port))))
  }
  const revoke = async (id: string, name: string) => {
    const ok = await confirmAction({ title: `Forget ${name}?`, description: "The phone disconnects. It needs a new code to pair again.", confirm: "Forget" })
    if (ok) await remote.revoke(id).then((s) => useRemote.setState({ state: s }), (e) => toast.error(String(e)))
  }

  if (!state) return null
  const tone = state.running ? "ok" : state.enabled ? "err" : "off"
  return (
    <Section
      title="Phone"
      description="Pair a phone to read chats, reply, and approve or stop turns. Paired phones connect directly over Wi-Fi or a VPN. Moonfish encrypts the connection end to end."
    >
      <Group>
        <Row
          label="Let phones connect"
          description={
            state.running
              ? `Listening on port ${state.port}${state.addresses.length ? ` · ${state.addresses.map((a) => a.ip).join(", ")}` : ""}`
              : state.error
                ? state.error
                : "Phone access is off. If Windows asks, allow Moonfish to use private networks."
          }
        >
          <Dot tone={tone} />
          <Switch checked={state.enabled} disabled={busy} onCheckedChange={toggle} aria-label="Let phones connect" />
        </Row>
        <Row label="Pair a phone" description="Enter this computer's address and code on your phone. The code expires after five minutes or one use.">
          <Button size="sm" disabled={!state.running} onClick={() => setPairing(true)}>
            <Smartphone />
            Pair a phone
          </Button>
        </Row>
        {navigator.userAgent.includes("Windows") ? (
          <Row label="Windows Firewall" description="Allow discovery and phone connections on Windows networks set to Private. Windows asks for administrator permission.">
            <Button size="sm" disabled={busy || !state.running} onClick={async () => {
              setBusy(true)
              try { await remote.allowFirewall(); toast.success("Firewall rules added. Tap Connect again on your phone.") }
              catch (e) { toast.error(String(e)) }
              finally { setBusy(false) }
            }}>Allow connections</Button>
          </Row>
        ) : null}
        {state.running && !state.discoveryAvailable ? <p className="px-4 py-2 text-[12px] text-warn">Nearby discovery could not start. You can still pair by address and code.</p> : null}
        <Row label="Port" description="Moonfish uses another port if this one is in use.">
          <Input
            type="number"
            min={1024}
            max={65535}
            className="h-7 w-24 text-right font-mono text-[12px]"
            value={port}
            onChange={(e) => setPort(e.target.value)}
            onBlur={() => void savePort()}
            onKeyDown={(e) => e.key === "Enter" && void savePort()}
            aria-label="Port"
          />
        </Row>
      </Group>

      <Group title="Paired phones" aside={state.devices.length ? <span className="text-[11px] text-faint">{state.devices.filter((d) => d.connected).length} connected</span> : null}>
        {state.devices.length === 0 ? (
          <div className="px-4 py-5 text-center text-[12.5px] text-muted-foreground">No phones yet.</div>
        ) : (
          state.devices.map((d) => (
            <Row
              key={d.id}
              label={
                <span className="flex items-center gap-2">
                  <Dot tone={d.connected ? "ok" : "off"} />
                  {d.name}
                </span>
              }
              description={d.connected ? "Connected now" : `Last seen ${seenLabel(d.lastSeen)} · paired ${new Date(d.pairedAt).toLocaleDateString()}`}
            >
              <Button size="xs" variant="outline" onClick={() => void revoke(d.id, d.name)}>
                Forget
              </Button>
            </Row>
          ))
        )}
      </Group>

      {state.addresses.some((a) => a.label === "Tailscale") ? null : (
        <p className="text-[12px] leading-relaxed text-muted-foreground">
          Away from home? Connect this computer and your phone to the same VPN, such as Tailscale, ZeroTier, or WireGuard. The VPN address appears here and in the pairing dialog.
        </p>
      )}

    </Section>
  )
}

/** Mounted once, so a phone's request is visible even outside Settings. */
export function PhonePairingHost() {
  const open = useRemote((s) => s.pairingOpen)
  const request = useRemote((s) => s.pairRequest)
  return <>
    <Dialog open={!!request && !open} onOpenChange={(o) => { if (!o) useRemote.setState({ pairRequest: null }) }}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Connect a phone?</DialogTitle>
          <DialogDescription>{request} wants to pair. Continue if you tapped Connect on that phone. Enter the code on the phone to finish.</DialogDescription>
        </DialogHeader>
        <div className="flex justify-end gap-2">
          <Button variant="outline" onClick={() => useRemote.setState({ pairRequest: null })}>Dismiss</Button>
          <Button onClick={() => useRemote.setState({ pairRequest: null, pairingOpen: true })}>Connect</Button>
        </div>
      </DialogContent>
    </Dialog>
    <PairDialog open={open} onOpenChange={(pairingOpen) => useRemote.setState({ pairingOpen })} />
  </>
}
