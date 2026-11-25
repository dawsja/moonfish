import { Check, Moon, Sun } from "lucide-react"
import type { Theme } from "@/lib/types"
import { cn } from "@/lib/utils"
import { AlertSettings } from "./Alerts"
import { setPhoneTheme, usePhoneTheme } from "./theme"
import { TitleCapsule, UNDER_CAPSULE } from "./ui"

const THEMES: { value: Theme; label: string; icon: typeof Sun }[] = [
  { value: "system", label: "System", icon: Check },
  { value: "dark", label: "Dark", icon: Moon },
  { value: "light", label: "Light", icon: Sun },
]

export function PhoneSettings() {
  const theme = usePhoneTheme((s) => s.theme)
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex gap-1 rounded-full bg-muted p-1" role="group" aria-label="Appearance">
        {THEMES.map(({ value, label, icon: Icon }) => (
          <button
            key={value}
            aria-pressed={theme === value}
            onClick={() => setPhoneTheme(value)}
            className={cn("flex h-10 flex-1 items-center justify-center gap-1.5 rounded-full text-[13.5px]", theme === value ? "bg-primary font-semibold text-primary-foreground" : "active:bg-accent")}
          >
            <Icon className={cn("size-4", theme !== value && "text-muted-foreground")} />
            {label}
          </button>
        ))}
      </div>
      <p className="px-3 pb-1 text-[12px] text-muted-foreground">System follows this phone's light or dark mode.</p>
      <AlertSettings />
    </div>
  )
}

export function SettingsScreen() {
  return (
    <div className="relative h-full bg-background">
      <div className={cn("h-full overflow-y-auto px-2.5 pb-8", UNDER_CAPSULE)}>
        <PhoneSettings />
      </div>
      <TitleCapsule title="Settings" subtitle="This phone" />
    </div>
  )
}
