import { useEffect, useState } from "react"
import { REGEXP_ONLY_DIGITS_AND_CHARS } from "input-otp"
import { ChevronLeft, Loader2 } from "lucide-react"
import { Mark } from "@/components/icons"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { InputOTP, InputOTPGroup, InputOTPSlot } from "@/components/ui/input-otp"
import { native, phoneName, type NearbyDesktop } from "./rpc"
import { back, go } from "./route"
import { PullSheet, SheetHead } from "./sheet"
import { useMobile } from "./store"
import { RoundButton } from "./ui"

export function PairScreen() {
  const hasDesktops = useMobile((s) => s.desktops.length > 0)
  const [address, setAddress] = useState("")
  const [code, setCode] = useState("")
  const [name, setName] = useState(phoneName)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [nearby, setNearby] = useState<NearbyDesktop[]>([])
  const [scanning, setScanning] = useState(false)
  const [scanError, setScanError] = useState<string | null>(null)
  const [selected, setSelected] = useState<NearbyDesktop | null>(null)
  const [requesting, setRequesting] = useState(false)
  const scan = async () => {
    setScanning(true)
    setScanError(null)
    try { setNearby(await native.discover()) }
    catch (e) { setScanError(String(e).replace(/^Error: /, "")) }
    finally { setScanning(false) }
  }
  useEffect(() => { void scan() }, [])
  const invite = async (desktop: NearbyDesktop) => {
    setAddress(desktop.address)
    setCode("")
    setError(null)
    setRequesting(true)
    try {
      await native.requestPairing(desktop.address, name)
      setSelected(desktop)
    } catch (e) { setError(String(e)) }
    finally { setRequesting(false) }
  }

  const done = async (id: string) => {
    await useMobile.getState().connect(id)
    go("/", true)
  }
  const pair = async () => {
    setError(null)
    setBusy(true)
    try {
      const d = await useMobile.getState().pair({ addresses: address.split(/[\s,]+/), code, name })
      await done(d.id)
    } catch (e) {
      setError(String(e).replace(/^Error: /, ""))
    } finally {
      setBusy(false)
    }
  }
  const canPair = address.trim().length > 0 && code.length === 8 && !busy
  const demo = async () => {
    setError(null)
    setBusy(true)
    try {
      const d = await useMobile.getState().startDemo()
      await done(d.id)
    } catch (e) {
      setError(String(e).replace(/^Error: /, ""))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="safe flex h-full flex-col overflow-y-auto bg-background">
      {hasDesktops ? (
        <div className="flex h-14 shrink-0 items-center px-3">
          <RoundButton label="Back" className="bg-muted" onClick={back}>
            <ChevronLeft className="size-5" />
          </RoundButton>
        </div>
      ) : (
        <div className="h-12 shrink-0" />
      )}
      <div className="mx-auto flex w-full max-w-sm flex-1 flex-col px-6 pb-8">
        <div className="mb-8 flex flex-col items-center text-center">
          <Mark size={44} label="Moonfish" />
          <h1 className="mt-4 text-[20px] font-semibold tracking-tight">Pair with your computer</h1>
          <p className="mt-1.5 text-[13px] leading-relaxed text-muted-foreground">
            Choose a nearby computer below. To pair by address, open Phone settings in Moonfish on the computer and select <span className="text-foreground">Pair a phone</span>.
          </p>
        </div>

        <div className="mb-6 rounded-[24px] bg-muted p-3.5">
          <div className="mb-1 flex items-center justify-between">
            <span className="pl-0.5 text-[13.5px] font-medium">Nearby computers</span>
            <button className="h-9 rounded-full px-3.5 text-[13px] text-muted-foreground active:bg-accent disabled:opacity-50" disabled={scanning || requesting || busy} onClick={() => void scan()}>{scanning ? "Searching…" : "Refresh"}</button>
          </div>
          {nearby.map((d) => <div key={d.address} className="flex items-center justify-between gap-2 py-1.5">
            <div className="min-w-0 pl-0.5"><div className="truncate text-[14px]">{d.name}</div><div className="font-mono text-[11.5px] text-muted-foreground">{d.address}</div></div>
            <button className="h-10 shrink-0 rounded-full bg-primary px-4 text-[13.5px] font-semibold text-primary-foreground disabled:opacity-50" disabled={requesting || busy} onClick={() => void invite(d)}>Connect</button>
          </div>)}
          {!scanning && !nearby.length ? <p className="pl-0.5 text-[12.5px] text-muted-foreground">{scanError ?? "No computers found. Turn on phone access on the computer or enter its address below."}</p> : null}
          <p className="mt-2 pl-0.5 text-[11.5px] text-faint">Guest Wi-Fi or a firewall may block discovery.</p>
        </div>

        <label className="mb-1.5 text-[12px] font-medium text-muted-foreground" htmlFor="addr">
          Address
        </label>
        <Input
          id="addr"
          inputMode="url"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          placeholder="192.168.1.20"
          className="h-12 rounded-full px-4 font-mono text-[14px]"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
        />
        <p className="mt-1 text-[11.5px] text-faint">Use the address shown on the computer. The port is optional.</p>

        <label className="mt-5 mb-1.5 text-[12px] font-medium text-muted-foreground">Code</label>
        {/* Eight boxes share the width (about 34px each on a 360px phone), a dash between the halves. */}
        <InputOTP
          maxLength={8}
          pattern={REGEXP_ONLY_DIGITS_AND_CHARS}
          // Codes have letters: a text keyboard (input-otp defaults to a number pad), capitals,
          // no autocorrect or suggestions.
          inputMode="text"
          autoCapitalize="characters"
          autoCorrect="off"
          autoComplete="off"
          spellCheck={false}
          value={code} onChange={(v) => setCode(v.toUpperCase())} containerClassName="w-full gap-1.5" aria-label="Pairing code">
          <InputOTPGroup className="min-w-0 flex-1">
            {[0, 1, 2, 3].map((i) => (
              <InputOTPSlot key={i} index={i} className="h-12 w-auto min-w-0 flex-1 font-mono text-[17px]" />
            ))}
          </InputOTPGroup>
          <span className="w-2.5 shrink-0 border-t-2 border-faint" aria-hidden />
          <InputOTPGroup className="min-w-0 flex-1">
            {[4, 5, 6, 7].map((i) => (
              <InputOTPSlot key={i} index={i} className="h-12 w-auto min-w-0 flex-1 font-mono text-[17px]" />
            ))}
          </InputOTPGroup>
        </InputOTP>

        <label className="mt-5 mb-1.5 text-[12px] font-medium text-muted-foreground" htmlFor="name">
          This phone's name
        </label>
        <Input id="name" className="h-12 rounded-full px-4 text-[14px]" value={name} onChange={(e) => setName(e.target.value)} />

        {error ? <div className="mt-5 rounded-[20px] border border-red/40 bg-red/5 px-4 py-2.5 text-[13px] leading-relaxed text-red">{error}</div> : null}

        <Button size="lg" className="mt-6 h-12 w-full rounded-full text-[15px] font-semibold disabled:bg-muted disabled:text-muted-foreground disabled:opacity-100" disabled={!canPair} onClick={() => void pair()}>
          {busy ? <Loader2 className="animate-spin" /> : null}
          {canPair || busy ? "Pair" : !address.trim() ? "Type the address and code" : "Type all 8 characters"}
        </Button>

        <button className="mx-auto mt-4 h-11 rounded-full px-5 text-center text-[13.5px] text-muted-foreground active:bg-accent disabled:opacity-50" disabled={busy} onClick={() => void demo()}>
          Try the demo
        </button>

        <p className="mt-auto pt-6 text-center text-[11.5px] leading-relaxed text-faint">
          Connect both devices to the same Wi-Fi network or VPN. Moonfish encrypts the connection end to end.
        </p>
      </div>
      <PullSheet
        open={!!selected}
        onClose={() => { if (!busy) setSelected(null) }}
        title={`Connect to ${selected?.name ?? "the computer"}`}
        description="Click Connect on the computer. Enter the code shown there."
        header={<SheetHead title={`Connect to ${selected?.name ?? ""}`} sub="Click Connect on the computer, then enter its code." />}
      >
        <div className="flex flex-col gap-2 px-3 pb-2">
          <Input aria-label="Desktop pairing code" placeholder="XXXX-XXXX" autoCapitalize="characters" autoCorrect="off" className="h-12 rounded-full px-4 font-mono text-[16px] tracking-widest" value={code} onChange={(e) => setCode(e.target.value.replace(/[^a-z0-9]/gi, "").toUpperCase().slice(0, 8))} />
          {error ? <p className="px-2 text-[12.5px] text-red">{error}</p> : null}
          <Button className="h-12 rounded-full text-[15px] font-semibold" disabled={!canPair} onClick={() => void pair()}>{busy ? "Connecting…" : "Connect"}</Button>
        </div>
      </PullSheet>
    </div>
  )
}
