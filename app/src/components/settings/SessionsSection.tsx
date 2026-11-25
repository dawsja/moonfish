import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { useApp } from "@/store/app"
import { Group, Row, Section } from "./layout"

export function SessionsSection() {
  const minutes = useApp((s) => s.settings!.agentSleepMinutes ?? 0)
  const update = useApp((s) => s.update)
  return (
    <Section title="Sessions" description="Sleep stops the agent to free memory. Send a message to resume the saved session.">
      <Group>
        <Row label="Sleep idle agents automatically" description="Only idle agents with a saved session can sleep, even in open chats. Agents stay awake while tasks run or while they wait for your response.">
          <Switch checked={minutes > 0} onCheckedChange={(on) => update((s) => void (s.agentSleepMinutes = on ? 30 : 0))} aria-label="Sleep idle agents automatically" />
        </Row>
        {minutes > 0 ? (
          <Row label="Sleep after" description="Minutes of inactivity after a turn ends. Moonfish checks every 15 seconds.">
            <Input key={minutes} type="number" min={1} max={1440} defaultValue={minutes} aria-label="Idle minutes before sleep" className="h-7 w-20 text-right font-mono text-[12px]"
              onBlur={(e) => {
                const value = Math.max(1, Math.min(1440, Math.round(Number(e.target.value) || 30)))
                e.target.value = String(value)
                void update((s) => void (s.agentSleepMinutes = value))
              }} />
            <span className="text-[12px] text-muted-foreground">minutes</span>
          </Row>
        ) : null}
      </Group>
      <p className="text-[12px] text-muted-foreground">You can also right-click a finished chat in the sidebar and choose Sleep. Agents without session resume support stay awake.</p>
    </Section>
  )
}
