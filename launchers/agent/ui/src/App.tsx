import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppearanceMenu } from "./components/AppearanceMenu";
import { DialogsProvider } from "./components/Dialogs";
import { Dock } from "./components/Dock";
import { FilesPanel } from "./components/FilesPanel";
import { Header, useTasks, type PanelName } from "./components/Header";
import { IconSprite } from "./components/Icon";
import { PanelBoundary } from "./components/PanelBoundary";
import { TasksPanel } from "./components/TasksPanel";
import { Thread } from "./components/Thread";
import { StoreProvider, useStore } from "./state/store";
import { t } from "./i18n";
import { FileNavigation } from "./state/files";

function Shell() {
  const { state } = useStore();
  const tasks = useTasks();
  const [menuOpen, setMenuOpen] = useState(false);
  const [panels, setPanels] = useState<Record<PanelName, boolean>>({ files: false, tasks: false });
  const [requestedFile, setRequestedFile] = useState<{ path: string; revision: number } | null>(null);
  const lookRef = useRef<HTMLButtonElement>(null);
  const triggers = useRef<Record<PanelName, HTMLElement | null>>({ files: null, tasks: null });
  const closeMenu = useCallback(() => setMenuOpen(false), []);
  const toggleMenu = useCallback(() => setMenuOpen((open) => !open), []);
  const name = state.snapshot?.harness.name || t("app.name");
  useEffect(() => { document.title = name; }, [name]);

  // Narrow screens show one full-screen panel at a time; wide screens can have both.
  const togglePanel = useCallback((panel: PanelName, trigger: HTMLElement) => {
    triggers.current[panel] = trigger;
    setPanels((current) => {
      const open = !current[panel];
      return matchMedia("(max-width: 900px)").matches ? { files: false, tasks: false, [panel]: open } : { ...current, [panel]: open };
    });
  }, []);
  const closePanel = useCallback((panel: PanelName) => {
    setPanels((current) => ({ ...current, [panel]: false }));
    triggers.current[panel]?.focus();
  }, []);
  useEffect(() => { if (!tasks.available) setPanels((current) => (current.tasks ? { ...current, tasks: false } : current)); }, [tasks.available]);
  const panelProps = { panels, onPanel: togglePanel };
  const openFile = useCallback((path: string, trigger: HTMLElement) => {
    triggers.current.files = trigger;
    setRequestedFile(current => ({ path, revision: (current?.revision ?? 0) + 1 }));
    setPanels(current => matchMedia("(max-width: 900px)").matches ? { files: true, tasks: false } : { ...current, files: true });
  }, []);
  const fileNavigation = useMemo(() => ({ cwd: state.snapshot?.thread?.cwd ?? "", openFile }), [state.snapshot?.thread?.cwd, openFile]);
  return (
    <FileNavigation.Provider value={fileNavigation}>
    <DialogsProvider>
      <div className="app" id="app">
        <IconSprite />
        <Header menuOpen={menuOpen} onToggleMenu={toggleMenu} lookRef={lookRef} {...panelProps} />
        <div className="main">
          <PanelBoundary><FilesPanel open={panels.files} requestedFile={requestedFile} onClose={() => closePanel("files")} /></PanelBoundary>
          <main className="pane">
            <Thread />
            <Dock />
          </main>
          {tasks.available && <PanelBoundary><TasksPanel open={panels.tasks} onClose={() => closePanel("tasks")} /></PanelBoundary>}
        </div>
        <AppearanceMenu open={menuOpen} onClose={closeMenu} />
      </div>
    </DialogsProvider>
    </FileNavigation.Provider>
  );
}

export function App() {
  return <StoreProvider><Shell /></StoreProvider>;
}
