// The app: rail + top bar + the page views + shell overlays, laid out as web/index.html (body is the grid; #root
// is display: contents).
//
// Page switching: every page is kept mounted inside <Activity>. The visible one is live; hidden ones are
// display:none with their effects (store subscriptions, timers, rAF loops) torn down, so they cost no CPU per
// update, and React pre-renders them at idle priority after boot. Switching back reveals already-built DOM
// instead of building a page (measured: see the report / docs). The Network sky and the Energy bank keep their
// paused WebGL context while hidden, so they show at once on return; the Weather radar is freed on hide and rebuilt
// on return (owner, 2026-11-24: its frames hold ~800 MB of GPU memory). lib/glkeep.ts; ?keepgl=1 / 0 keep / free all. A page that is expensive to even pre-render (maps, three.js) should be React.lazy() and
// do its heavy setup in an effect.
import { Activity, lazy, memo, Suspense, useEffect, type ComponentType } from "react";
import { ConfirmModal, Toast, WakeShield } from "./components/Overlays";
import { Rail, TopBar } from "./components/Shell";
import { BattModal, InvModal } from "./pages/home/Overlays";
import HomePage from "./pages/home/HomePage";
import SwitchesPage from "./pages/switches/SwitchesPage";
import AlertsPage from "./pages/alerts/AlertsPage";
import { go, useView, VIEWS, type ViewId } from "./state/route";

// Each page lives in its own directory (src/pages/<id>/); the heavier ones load as their own chunks.
const PAGES: Record<ViewId, ComponentType> = {
  home: HomePage,
  energy: lazy(() => import("./pages/energy/EnergyPage")),
  climate: lazy(() => import("./pages/climate/ClimatePage")),
  weather: lazy(() => import("./pages/weather/WeatherPage")),
  switches: SwitchesPage,
  network: lazy(() => import("./pages/network/NetworkPage")),
  alerts: AlertsPage,
  systems: lazy(() => import("./pages/systems/SystemsPage")),
  settings: lazy(() => import("./pages/settings/SettingsPage")),
};

const View = memo(function View({ id, on }: { id: ViewId; on: boolean }) {
  const Page = PAGES[id];
  return (
    <Activity mode={on ? "visible" : "hidden"}>
      <section className={`view${on ? " on" : ""}`} data-view={id}>
        <Suspense fallback={<div className="page-loading" />}><Page /></Suspense>
      </section>
    </Activity>
  );
});

// ?keep=0: mount only the visible page (the alternative strategy, kept for benchmarking the runtime choice)
const KEEP_MOUNTED = typeof location === "undefined" || new URLSearchParams(location.search).get("keep") !== "0";

const Views = memo(function Views() {
  const view = useView();
  return (
    <main id="views">
      {VIEWS.map(([id]) => KEEP_MOUNTED || id === view ? <View key={id} id={id} on={id === view} /> : null)}
    </main>
  );
});

/** A tap on a [data-goto] card navigates, unless it landed on (or beside) a control inside it (app.js). */
function useGotoClicks() {
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      const t = e.target as Element | null;
      const g = t?.closest<HTMLElement>("[data-goto]");
      if (!g) return;
      // even a momentarily-disabled control (a command pending) must never let the card navigate
      const ctl = t!.closest("button, input, select, a, .home-sp, .no-goto");
      if (ctl && ctl !== g && g.contains(ctl)) return;
      go(g.dataset.goto as ViewId);
    };
    document.addEventListener("click", onClick);
    return () => document.removeEventListener("click", onClick);
  }, []);
}

export default function App() {
  useGotoClicks();
  return (
    <>
      <Rail />
      <div className="shell">
        <TopBar />
        <Views />
      </div>
      <Toast />
      <BattModal />
      <InvModal />
      <ConfirmModal />
      <WakeShield />
    </>
  );
}
