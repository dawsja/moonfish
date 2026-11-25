// Text handed to the chat from elsewhere in the window: a file mention from the side panel,
// or notes on lines of a diff.

import { formatNotes } from "@/lib/notes"
import { useApp } from "./app"
import { useNotes } from "./notes"

export type InsertDetail = { text: string }

export function insertIntoChat(text: string) {
  const app = useApp.getState()
  if (app.view !== "chat") app.closeSettings()
  if (app.viewer) app.setActive({ viewer: null })
  // Once the composer is there (it isn't while a file is shown).
  requestAnimationFrame(() => requestAnimationFrame(() => window.dispatchEvent(new CustomEvent<InsertDetail>("moonfish:insert", { detail: { text } }))))
}

/** The workspace's line notes, all in one draft for the active chat (never sent from
 *  here). They're done with once they're in it. */
export function insertNotes(scopeKey: string) {
  const text = formatNotes(useNotes.getState().take(scopeKey))
  if (text) insertIntoChat(text)
}
