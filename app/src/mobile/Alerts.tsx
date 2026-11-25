// Settings → Alerts: a notification when a chat on the desktop needs you (phone/src-tauri/src/
// alerts.rs decides; the Android side shows it). Off until turned on: nothing runs in the
// background and Android isn't asked for notifications until then.

import { useEffect, useState } from "react"
import { isTauri } from "@tauri-apps/api/core"
import { toast } from "sonner"
import { Switch } from "@/components/ui/switch"
import { native, type AlertsState } from "./rpc"

/** What to say about Android's notification permission, if anything. */
export function permissionNote(s: AlertsState, tried: boolean): string | null {
  if (s.permission === "granted" || s.permission === "unsupported") return null
  if (s.enabled) return "Android has notifications off for Moonfish, so alerts can't show. Allow them in Android's settings."
  if (!tried) return null
  return s.permission === "denied"
    ? "Android has notifications off for Moonfish. Allow them in Android's settings, then turn alerts on."
    : "Alerts need notifications. Turn alerts on again and choose Allow."
}

export function AlertSettings() {
  const [state, setState] = useState<AlertsState | null>(null)
  const [changing, setChanging] = useState(false)
  const [tried, setTried] = useState(false)

  useEffect(() => {
    if (!isTauri()) return
    const load = () => void native.alertsState().then(setState, () => setState(null))
    load()
    // Back from Android's settings: notifications may be allowed now.
    const onVisible = () => {
      if (document.visibilityState === "visible") load()
    }
    document.addEventListener("visibilitychange", onVisible)
    return () => document.removeEventListener("visibilitychange", onVisible)
  }, [])

  const change = async (o: { enabled?: boolean; done?: boolean }) => {
    if (changing) return
    setChanging(true)
    if (o.enabled) setTried(true)
    try {
      setState(await native.alertsSet(o))
    } catch (e) {
      toast.error("Couldn't change alerts", { description: String(e) })
    } finally {
      setChanging(false)
    }
  }

  const note = state ? permissionNote(state, tried) : null
  return (
    <section className="rounded-[24px] bg-muted px-4 py-3.5">
      <h3 className="text-[14.5px] font-medium">Alerts</h3>
      <p className="mt-0.5 mb-2.5 text-[12.5px] leading-snug text-muted-foreground">
        Get notified when a chat on your desktop needs an approval, a question, or a plan review. While a chat runs, Moonfish stays connected in the background with a "Following chats" notification, and disconnects once every chat is idle. Swiping Moonfish away from recent apps stops alerts.
      </p>
      {!isTauri() ? (
        <p className="text-[13px] text-muted-foreground">Available in the phone app.</p>
      ) : !state ? null : state.permission === "unsupported" ? (
        <p className="text-[13px] text-muted-foreground">Alerts need the Android app.</p>
      ) : (
        <div className="flex flex-col gap-1.5">
          <Switch className="flex w-full rounded-[20px] bg-background px-4 py-3 text-[13.5px]" aria-label="Alert me when a chat needs me" checked={state.enabled} disabled={changing} onCheckedChange={(enabled) => void change({ enabled })}>
            <span>When a chat needs me</span>
          </Switch>
          {state.enabled ? (
            <Switch className="flex w-full rounded-[20px] bg-background px-4 py-3 text-[13.5px]" aria-label="Also when a chat is done" checked={state.done} disabled={changing} onCheckedChange={(done) => void change({ done })}>
              <span>
                Also when a chat is done
                <span className="block text-[12px] text-muted-foreground">Notify when a turn finishes or stops with an error</span>
              </span>
            </Switch>
          ) : null}
          {note ? (
            <div className="mt-1 flex flex-col items-start gap-2" role="status">
              <p className="text-[13px]">{note}</p>
              {state.permission === "denied" ? (
                <button className="h-10 rounded-full bg-background px-4 text-[13px] font-medium active:bg-accent" onClick={() => void native.alertsSettings().catch((e) => toast.error(String(e)))}>
                  Open Android settings
                </button>
              ) : null}
            </div>
          ) : null}
        </div>
      )}
    </section>
  )
}
