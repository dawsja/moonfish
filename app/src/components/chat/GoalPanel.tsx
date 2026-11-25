import { useId, useRef, useState } from "react"
import { CircleCheck, Eye, Flag, LoaderCircle, MoreHorizontal, Pause, Pencil, Play, Target, Trash2 } from "lucide-react"
import { toast } from "sonner"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import type { Goal, GoalAction, GoalInput } from "@/lib/goals"
import { cn } from "@/lib/utils"

const statuses: Record<Goal["status"], string> = {
  active: "Goal active",
  paused: "Goal paused",
  blocked: "Goal blocked",
  usageLimited: "Usage limit reached",
  budgetLimited: "Goal budget reached",
  complete: "Goal complete",
}

function GoalHeader({ goal, running, waiting, busy, disabled, act, edit }: {
  goal: Goal
  running: boolean
  waiting: boolean
  busy: boolean
  disabled: boolean
  act: (action: GoalAction) => void
  edit: (trigger: HTMLElement | null) => void
}) {
  const active = goal.status === "active"
  const complete = goal.status === "complete"
  const status = active ? waiting ? "Goal needs your response" : running ? "Goal running" : statuses.active : statuses[goal.status]
  const Icon = complete ? CircleCheck : active ? (waiting ? Flag : Target) : Pause
  const menuTrigger = useRef<HTMLButtonElement>(null)
  const openingObjective = useRef(false)
  return (
    <section aria-label="Goal" aria-busy={busy} className={cn(
      "flex items-start gap-2.5 rounded-t-2xl border-b bg-muted/50 px-3 py-2.5",
      active && "border-primary/20 bg-primary/7",
    )}>
      <span aria-hidden="true" className={cn(
        "grid size-8 shrink-0 place-items-center self-center rounded-md border bg-background text-muted-foreground",
        active && "border-primary/25 bg-primary/10 text-yellow dark:text-primary", complete && "text-green",
      )}>
        {busy ? <LoaderCircle className="size-4 animate-spin" /> : <Icon className="size-4" />}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5">
          <span className={cn("text-[11px] font-semibold", active ? "text-yellow dark:text-primary" : complete ? "text-green" : "text-muted-foreground")} role="status">{status}</span>
          {goal.tokensUsed !== undefined && goal.tokensUsed > 0 ? <span className="text-[11px] text-faint">
            {goal.tokensUsed.toLocaleString()}{goal.tokenBudget ? ` / ${goal.tokenBudget.toLocaleString()}` : ""} tokens
          </span> : null}
        </div>
        <p className="line-clamp-3 whitespace-pre-wrap text-[12.5px] leading-snug [overflow-wrap:anywhere]" title={goal.objective}>{goal.objective}</p>
        {goal.note ? <p className="mt-1 text-[11px] text-muted-foreground">{goal.note}</p> : null}
      </div>
      <div className="flex shrink-0 items-center gap-0.5 self-center">
        {active ? (
          <Button size="xs" variant="outline" className="h-8 pointer-coarse:h-10" disabled={disabled || busy} onClick={() => act({ action: "pause" })}><Pause /> Pause</Button>
        ) : !complete ? (
          <Button size="xs" variant="outline" className="h-8 pointer-coarse:h-10" disabled={disabled || busy || running} onClick={() => act({ action: "resume" })}><Play /> Resume</Button>
        ) : null}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button ref={menuTrigger} size="icon-sm" variant="ghost" className="text-muted-foreground pointer-coarse:size-10" aria-label="Goal options" disabled={disabled || busy}>
              <MoreHorizontal />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" onCloseAutoFocus={(event) => {
            if (openingObjective.current) event.preventDefault()
            openingObjective.current = false
          }}>
            <DropdownMenuItem onSelect={() => {
              openingObjective.current = true
              edit(menuTrigger.current)
            }}>
              {active ? <><Eye /> View objective</> : <><Pencil /> Edit goal</>}
            </DropdownMenuItem>
            {active && !running && !waiting ? (
              <DropdownMenuItem onSelect={() => act({ action: "resume" })}><Play /> Resume goal</DropdownMenuItem>
            ) : null}
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onSelect={() => act({ action: "clear" })}><Trash2 /> Clear goal</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </section>
  )
}

export function useGoalControls(goal: Goal | null | undefined, run: (action: GoalAction) => Promise<boolean>, {
  running = false, waiting = false, disabled = false,
}: { running?: boolean; waiting?: boolean; disabled?: boolean } = {}) {
  const [editing, setEditing] = useState(false)
  const [objective, setObjective] = useState("")
  const [busy, setBusy] = useState(false)
  const inputId = useId()
  const returnFocus = useRef<HTMLElement | null>(null)
  const edit = (trigger: HTMLElement | null) => {
    returnFocus.current = trigger
    setObjective(goal?.objective ?? "")
    setEditing(true)
  }
  const perform = async (action: GoalAction) => {
    if (busy || disabled) return false
    setBusy(true)
    try {
      return await run(action)
    } catch (e) {
      toast.error("Couldn't update the goal", { description: String(e) })
      return false
    } finally {
      setBusy(false)
    }
  }
  const command = (input: GoalInput) => {
    if (input.action === "view" || input.action === "edit") {
      edit(document.activeElement instanceof HTMLElement ? document.activeElement : null)
      return Promise.resolve(true)
    }
    if (input.action === "set" || input.action === "pause" || input.action === "resume" || input.action === "clear") return perform(input)
    return Promise.resolve(false)
  }
  const active = goal?.status === "active"
  return {
    active,
    command,
    header: goal ? <GoalHeader goal={goal} running={running} waiting={waiting} busy={busy} disabled={disabled} act={(action) => void perform(action)} edit={edit} /> : null,
    editor: <Dialog open={editing} onOpenChange={(open) => { if (!busy) setEditing(open) }}>
      <DialogContent className="sm:max-w-lg" showCloseButton={!busy} onCloseAutoFocus={(event) => {
        if (!returnFocus.current?.isConnected) return
        event.preventDefault()
        returnFocus.current.focus({ preventScroll: true })
      }}>
        <DialogHeader>
          <DialogTitle>{active ? "Current goal" : goal ? "Edit goal" : "Start a goal"}</DialogTitle>
          <DialogDescription>{active
            ? "Pause this goal to change its objective."
            : "State what the agent should finish and how it can verify the result."}</DialogDescription>
        </DialogHeader>
        <form className="space-y-3" onSubmit={(event) => {
          event.preventDefault()
          if (active || !objective.trim()) return
          void perform({ action: "set", objective }).then((ok) => { if (ok) setEditing(false) })
        }}>
          <label htmlFor={inputId} className="text-[12px] font-medium">Objective</label>
          <textarea
            id={inputId}
            className="min-h-28 w-full resize-y rounded-md border bg-card p-3 text-[13px] leading-relaxed focus-visible:outline-2 focus-visible:outline-ring"
            placeholder="Fix the failing tests and verify that the full suite passes."
            value={objective}
            maxLength={4000}
            onChange={(event) => setObjective(event.target.value)}
            readOnly={active || busy}
            required
          />
          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={() => setEditing(false)}>{active ? "Close" : "Cancel"}</Button>
            {!active ? <Button type="submit" disabled={busy || !objective.trim()}>{busy ? "Saving goal" : "Start goal"}</Button> : null}
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>,
  }
}
