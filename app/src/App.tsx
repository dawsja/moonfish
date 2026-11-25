import { cn } from "@/lib/utils"
import { useEffect, useState } from "react"
import { PhonePairingHost } from "@/components/settings/PhoneSection"
import { PromptHost } from "@/components/Prompt"
import { ImageLightbox } from "@/components/ImageView"
import { RemoteFolderHost } from "@/components/settings/RemoteFolder"
import { Toaster } from "@/components/ui/sonner"
import { TooltipProvider } from "@/components/ui/tooltip"
import { ChatView } from "@/components/chat/ChatView"
import { SettingsPage } from "@/components/settings/SettingsPage"
import { Resizer } from "@/components/shell/Resizer"
import { Sidebar } from "@/components/shell/Sidebar"
import { TitleBar } from "@/components/shell/TitleBar"
import { ErrorBoundary } from "@/components/ErrorBoundary"
import { Button } from "@/components/ui/button"
import { defaultLayout, useApp } from "@/store/app"
import { useChats } from "@/store/chats"
import { useProviders } from "@/store/providers"
import { AGENTS } from "@/lib/types"
import { initRemote } from "@/lib/remote"
import { initAttention } from "@/lib/attention"
import { ChatSearch, openChatSearch } from "@/components/shell/ChatSearch"

export function App() {
  const load = useApp((s) => s.load)
  const ready = useApp((s) => !!s.settings)
  const error = useApp((s) => s.error)
  const view = useApp((s) => s.view)
  const layout = useApp((s) => s.layout)
  const setLayout = useApp((s) => s.setLayout)
  const [resizing, setResizing] = useState(false)

  useEffect(() => {
    load().then(() => {
      const settings = useApp.getState().settings
      if (settings) void useProviders.getState().probeLocal(settings.providers)
      void useChats.getState().init().then(initAttention)
      initRemote()
    })
  }, [load])

  useEffect(() => {
    const onFocus = () => {
      const providers = useApp.getState().settings?.providers
      if (!providers) return
      const local = useProviders.getState().byLocation.local
      if (!local) return
      for (const a of AGENTS) {
        if (local.probes[a] && !local.probes[a]!.found) {
          void useProviders.getState().probeLocalAgent(a, providers[a].binary)
        }
      }
    }
    window.addEventListener("focus", onFocus)
    return () => window.removeEventListener("focus", onFocus)
  }, [])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const s = useApp.getState()
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && (e.key === "k" || e.key === "K")) {
        e.preventDefault()
        openChatSearch()
      } else if (e.ctrlKey && (e.key === "n" || e.key === "N")) {
        e.preventDefault()
        s.setActive({ chatId: null, draftCwd: null, viewer: null })
        useApp.setState({ view: "chat" })
      } else if (e.key === "Escape" && s.view === "settings" && !e.defaultPrevented) {
        // A menu, select or dialog inside Settings closes first (Radix marks its Esc handled).
        s.closeSettings()
      }
    }
    window.addEventListener("keydown", onKey)
    return () => window.removeEventListener("keydown", onKey)
  }, [])

  return (
    <TooltipProvider delayDuration={400}>
      <div className="flex h-full min-h-0 flex-col overflow-clip bg-sidebar">
        <TitleBar />
        {ready ? (
          <div className="flex min-h-0 flex-1 pr-2 pb-2">
            <div
              aria-hidden={!layout.sidebarOpen}
              inert={!layout.sidebarOpen}
              style={{ width: layout.sidebarOpen ? layout.sidebarWidth : 0 }}
              className={cn(
                "flex shrink-0 justify-end overflow-hidden",
                !resizing && "transition-[width,opacity] duration-[220ms] ease-[cubic-bezier(0.22,1,0.36,1)]",
                !layout.sidebarOpen && "pointer-events-none opacity-0",
              )}
            >
              <div style={{ width: layout.sidebarWidth }} className="h-full shrink-0">
                <ErrorBoundary label="the sidebar">
                  <Sidebar />
                </ErrorBoundary>
              </div>
            </div>
            <Resizer
              side="left"
              defaultWidth={defaultLayout.sidebarWidth}
              width={layout.sidebarWidth}
              min={200}
              max={460}
              disabled={!layout.sidebarOpen}
              onDragChange={setResizing}
              onWidth={(w) => setLayout({ sidebarWidth: w })}
            />
            <main className="flex min-w-0 flex-1 flex-col">
              {view === "settings" ? (
                <div className="min-h-0 flex-1 overflow-clip rounded-lg border bg-background">
                  <ErrorBoundary label="Settings">
                    <SettingsPage />
                  </ErrorBoundary>
                </div>
              ) : (
                <section className="relative min-h-0 min-w-0 flex-1 overflow-clip rounded-lg border bg-background">
                  <ErrorBoundary label="this chat">
                    <ChatView />
                  </ErrorBoundary>
                </section>
              )}
            </main>
          </div>
        ) : error ? (
          // Settings couldn't be loaded at all: say so, instead of an empty window.
          <div role="alert" className="flex flex-1 flex-col items-center justify-center gap-3 bg-background p-6 text-center">
            <p className="text-[13px] font-medium">Moonfish couldn't start.</p>
            <p className="max-w-md font-mono text-[11.5px] break-all text-muted-foreground">{error}</p>
            <Button size="sm" variant="outline" onClick={() => void load()}>
              Try again
            </Button>
          </div>
        ) : (
          <div className="flex-1 bg-background" />
        )}
      </div>
      <Toaster position="bottom-right" />
      <PromptHost />
      <PhonePairingHost />
      <ImageLightbox inWindow />
      <ChatSearch />
      <RemoteFolderHost />
    </TooltipProvider>
  )
}
