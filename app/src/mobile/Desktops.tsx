import { useEffect, useState } from "react"
import { Monitor, MonitorSmartphone, Plus, Trash2 } from "lucide-react"
import { cn } from "@/lib/utils"
import { go } from "./route"
import { useMobile } from "./store"
import { RoundButton, TitleCapsule, UNDER_CAPSULE } from "./ui"

/** The paired desktops: tap one to switch to it, forget one with a second tap, or pair another.
 *  `onPicked`: after switching (the capsule's sheet closes). */
export function DesktopList({ onPicked }: { onPicked?: () => void }) {
  const desktops = useMobile((s) => s.desktops)
  const current = useMobile((s) => s.desktopId)
  const link = useMobile((s) => s.link)
  // Forgetting asks with a second tap (Android's WebView may not show confirm dialogs).
  const [armed, setArmed] = useState<string | null>(null)
  useEffect(() => {
    if (!armed) return
    const t = setTimeout(() => setArmed(null), 3000)
    return () => clearTimeout(t)
  }, [armed])

  const forget = async (id: string) => {
    if (armed !== id) return setArmed(id)
    setArmed(null)
    await useMobile.getState().forget(id)
  }

  return (
    <div className="flex flex-col gap-1">
      {desktops.map((d) => {
        const on = d.id === current
        const Icon = on ? Monitor : MonitorSmartphone
        return (
          <div key={d.id} className={cn("flex items-center gap-1 rounded-[24px] p-1.5", on && "bg-muted")}>
            <button
              className="flex min-w-0 flex-1 items-center gap-3 rounded-[20px] p-1 text-left active:bg-accent"
              onClick={async () => {
                onPicked?.()
                await useMobile.getState().connect(d.id)
                go("/", true)
              }}
            >
              <span className={cn("grid size-11 shrink-0 place-items-center rounded-full", on ? "bg-background" : "bg-muted")}>
                <Icon className={cn("size-5", on ? "text-foreground" : "text-muted-foreground")} />
              </span>
              <span className="min-w-0 flex-1">
                <span className={cn("block truncate text-[15px]", on && "font-semibold")}>{d.name}</span>
                <span className={cn("block truncate text-[12px] text-muted-foreground", !d.demo && "font-mono text-[11.5px]")}>{d.demo ? "A demo inside this app" : (d.lastAddress ?? d.addresses[0])}</span>
              </span>
              {on ? (
                <span className={cn("flex shrink-0 items-center gap-1.5 text-[12px]", link === "connected" ? "text-green" : link === "connecting" ? "text-muted-foreground" : "text-red")}>
                  <span className={cn("size-1.5 rounded-full", link === "connected" ? "bg-green" : link === "connecting" ? "bg-muted-foreground" : "bg-red")} />
                  {link === "connected" ? "Connected" : link === "connecting" ? "Connecting…" : "Offline"}
                </span>
              ) : null}
            </button>
            {armed === d.id ? (
              <button className="h-9 shrink-0 rounded-full border border-red bg-red/10 px-3.5 text-[13px] text-red" onClick={() => void forget(d.id)}>
                Forget
              </button>
            ) : (
              <RoundButton label={`Forget ${d.name}`} onClick={() => void forget(d.id)} className="size-9">
                <Trash2 className="size-4" />
              </RoundButton>
            )}
          </div>
        )
      })}
      <button className="mt-1 flex h-11 w-full items-center justify-center gap-2 rounded-full border border-dashed border-input text-[14px] active:bg-accent" onClick={() => go("/pair")}>
        <Plus className="size-4" /> Pair a desktop
      </button>
    </div>
  )
}

/** The paired desktops, as a screen: shown when none is picked (after forgetting the one in use). */
export function DesktopsScreen() {
  const current = useMobile((s) => s.desktopId)
  return (
    <div className="relative h-full bg-background">
      <div className={cn("h-full overflow-y-auto px-2.5 pb-8", UNDER_CAPSULE)}>
        <DesktopList />
        <p className="mt-3 text-center text-[12px] text-faint">Tap a desktop to switch to it.</p>
      </div>
      <TitleCapsule title="Desktops" subtitle={current ? undefined : "Pick one to connect"} onBack={current ? undefined : null} />
    </div>
  )
}
