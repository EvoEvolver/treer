import { useCallback, useEffect, useRef, useState } from "react";
import { AppearanceMenu } from "./components/AppearanceMenu";
import { Dock } from "./components/Dock";
import { Header, Rail, Tabbar } from "./components/Header";
import { IconSprite } from "./components/Icon";
import { Thread } from "./components/Thread";
import { StoreProvider, useStore } from "./state/store";
import { t } from "./i18n";

function Shell() {
  const { state } = useStore();
  const [menuOpen, setMenuOpen] = useState(false);
  const lookRef = useRef<HTMLButtonElement>(null);
  const close = useCallback(() => setMenuOpen(false), []);
  const toggle = useCallback(() => setMenuOpen((open) => !open), []);
  const name = state.snapshot?.harness.name || t("app.name");
  useEffect(() => { document.title = name; }, [name]);
  return (
    <div className="app" id="app">
      <IconSprite />
      <Header menuOpen={menuOpen} onToggleMenu={toggle} lookRef={lookRef} />
      <Rail menuOpen={menuOpen} onToggleMenu={toggle} />
      <div className="main">
        <main className="pane">
          <Thread />
          <Dock />
        </main>
      </div>
      <AppearanceMenu open={menuOpen} onClose={close} />
      <Tabbar menuOpen={menuOpen} onToggleMenu={toggle} />
    </div>
  );
}

export function App() {
  return <StoreProvider><Shell /></StoreProvider>;
}
