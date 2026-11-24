import "./styles/app.css";
import "./styles/motion.css";
import "./styles/responsive.css";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { prefs } from "./lib/prefs";
import { installPressFeedback } from "./lib/press";
import { installAlertLogic } from "./logic/alerts";
import { installKiosk } from "./logic/kiosk";
import { installSwitchLogic } from "./logic/switches";
import { openModal } from "./logic/ui";
import { K, store } from "./state/store";
import { gateway } from "./state/ws";

const qs = new URLSearchParams(location.search);
const html = document.documentElement;
const night = qs.get("night") === "1" || prefs.get("vp.night", "0") === "1";
html.classList.toggle("night", night);
store.set(K.night, night);

// Fast boot: the shell and every card render at once as skeletons; values fill in when the snapshot lands.
html.classList.add("booting");
store.subscribe(K.ready, () => {
  const ready = store.get(K.ready) === true;
  html.classList.toggle("booting", !ready);
  // boot timing marks for the runtime benchmark (prompt §7): the frame that first shows live data
  if (ready && !performance.getEntriesByName("aetherrv:data-frame").length) {
    performance.mark("aetherrv:snapshot");
    requestAnimationFrame(() => setTimeout(() => performance.mark("aetherrv:data-frame"), 0));
  }
});

installPressFeedback();
installSwitchLogic();
installAlertLogic();
installKiosk();
gateway.start();

createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);

// screenshot / debug aids (as app.js): ?inv=1, ?batt=1 open the overlays
if (qs.get("inv") === "1") setTimeout(() => openModal("inv"), 2500);
if (qs.get("batt") === "1") setTimeout(() => openModal("batt"), 2500);
if (qs.get("bench") === "1") void import("./bench");   // runtime benchmark instrumentation (harness/uibench), own chunk
