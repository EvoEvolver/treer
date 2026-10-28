import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Composer } from "./components/Composer";
import { DialogsProvider } from "./components/Dialogs";
import { FilesPanel } from "./components/FilesPanel";
import { Header, useTasks, type DrawerName } from "./components/Header";
import { IconSprite } from "./components/Icon";
import { PanelBoundary } from "./components/PanelBoundary";
import { Settings } from "./components/Settings";
import { TasksPanel } from "./components/TasksPanel";
import { Thread } from "./components/Thread";
import { StoreProvider, useStore } from "./state/store";
import { FileNavigation } from "./state/files";

function Shell() {
  const { state } = useStore();
  const tasks = useTasks();
  const [drawer, setDrawer] = useState<DrawerName | null>(null);
  const [settings, setSettings] = useState(false);
  const [requestedFile, setRequestedFile] = useState<{ path: string; revision: number } | null>(null);
  const triggers = useRef<{ drawer: HTMLElement | null; settings: HTMLElement | null }>({ drawer: null, settings: null });

  // One drawer at a time: beside the conversation on wide screens, over it on narrow ones.
  const toggleDrawer = useCallback((name: DrawerName, trigger: HTMLElement) => {
    triggers.current.drawer = trigger;
    setDrawer((current) => (current === name ? null : name));
  }, []);
  const closeDrawer = useCallback(() => {
    setDrawer(null);
    triggers.current.drawer?.focus();
  }, []);
  const toggleSettings = useCallback((trigger: HTMLElement) => { triggers.current.settings = trigger; setSettings((open) => !open); }, []);
  const closeSettings = useCallback((returnFocus: boolean) => { setSettings(false); if (returnFocus) triggers.current.settings?.focus(); }, []);
  useEffect(() => { if (!tasks.available) setDrawer((current) => (current === "tasks" ? null : current)); }, [tasks.available]);
  const openFile = useCallback((path: string, trigger: HTMLElement) => {
    triggers.current.drawer = trigger;
    setRequestedFile((current) => ({ path, revision: (current?.revision ?? 0) + 1 }));
    setDrawer("files");
  }, []);
  const cwd = state.snapshot?.thread?.cwd ?? state.staleThread?.cwd ?? "";
  const fileNavigation = useMemo(() => ({ cwd, openFile }), [cwd, openFile]);
  return (
    <FileNavigation.Provider value={fileNavigation}>
      <DialogsProvider>
        <div className="app" data-drawer={drawer ?? undefined}>
          <IconSprite />
          <Header drawer={drawer} onDrawer={toggleDrawer} settingsOpen={settings} onSettings={toggleSettings} />
          <div className="main">
            <main className="pane">
              <Thread />
              <Composer />
            </main>
            <PanelBoundary><FilesPanel open={drawer === "files"} requestedFile={requestedFile} onClose={closeDrawer} /></PanelBoundary>
            {tasks.available && <PanelBoundary><TasksPanel open={drawer === "tasks"} onClose={closeDrawer} /></PanelBoundary>}
          </div>
          <Settings open={settings} onClose={closeSettings} />
        </div>
      </DialogsProvider>
    </FileNavigation.Provider>
  );
}

export function App() {
  return <StoreProvider><Shell /></StoreProvider>;
}
