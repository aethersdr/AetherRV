// Weather (web/index.html #weather + app.js renderWeather / renderWxLoc / renderForecast): the radar map with its
// layers menu, opacity slider, zoom, re-centre, loop length / speed and timeline; the sun line and the van's
// position in the header; the 7-day forecast for the van (or a tapped spot) beside the map.
//
// The map is imperative (wxmap.ts): React renders its controls once and the controller writes the per-frame parts
// (timeline, label) straight into them. Small sync components feed it the van position and the traffic state, so
// a GPS update re-renders a few lines, never the page. Nothing here sends a device command.
import "leaflet/dist/leaflet.css";
import "./weather.css";
import { memo, useCallback, useEffect, useRef, useState, type PointerEvent as RPointerEvent } from "react";
import { Icon } from "../../components/Icon";
import { prefs } from "../../lib/prefs";
import { useKey, useReady, useSelect } from "../../state/hooks";
import { K, store } from "../../state/store";
import { RV_SPANS, RV_SPEEDS } from "./frames";
import { loadForecast, forecastRows, resetForecast, useForecast } from "./forecast";
import { darkMapOn, fmtLL, mapModeTitle, sunTimes, WX_MODES, wxSun, type LatLon, type MapMode } from "./geo";
import { afterPaint, P, wxPrefs, WxMap, type WxBase, type WxEls } from "./wxmap";
import type { WxLayer } from "./frames";

const LOC = ["location.lat", "location.lon"] as const;

/** Re-render once a minute (the sun line's "now" marker, sunrise/sunset crossings). Effect-owned: stops when hidden. */
function useMinute(): number {
  const [t, setT] = useState(() => Date.now());
  useEffect(() => {
    setT(Date.now());
    const h = setInterval(() => setT(Date.now()), 60e3);
    return () => clearInterval(h);
  }, []);
  return t;
}

// ── header ──
const hm = (t: number) => new Date(t).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/** Sunrise ▬ sunset with the daylight length above the lit span and a "now" marker. */
const WxLoc = memo(function WxLoc() {
  const now = useMinute();
  const v = useSelect(["location.lat", "location.lon", "location.hdop", "location.source"], g => ({
    lat: g<number | null>("location.lat") ?? null, lon: g<number | null>("location.lon") ?? null,
    title: [g("location.hdop") != null ? `HDOP ${g("location.hdop")}` : null, g("location.source") ? `${g("location.source")} GPS` : null].filter(Boolean).join(" · "),
  }));
  const st = v.lat != null && v.lon != null ? sunTimes(+v.lat, +v.lon, new Date(now)) : null;
  if (!st) return <div className="wx-loc" id="wx-loc" title={v.title} />;
  const d0 = new Date(now); d0.setHours(0, 0, 0, 0);
  const pct = (t: number) => Math.max(0, Math.min(100, (t - d0.getTime()) / 864000));
  const len = Math.round((st.set - st.rise) / 60000), a = pct(st.rise), b = pct(st.set);
  return (
    <div className="wx-loc" id="wx-loc" title={v.title}>
      <b>{hm(st.rise)}<small>sunrise</small></b>
      <div className="daybar"><i style={{ left: `${a}%`, width: `${b - a}%` }} /><b style={{ left: `${pct(now)}%` }} />
        <em style={{ left: `${(a + b) / 2}%` }}>{`${Math.floor(len / 60)} h ${len % 60} m daylight`}</em></div>
      <b>{hm(st.set)}<small>sunset</small></b>
    </div>
  );
});

/** "Van 45.123° N, 110.456° W" and altitude · moving / parked. */
const WxSub = memo(function WxSub() {
  const ready = useReady();
  const v = useSelect(["location.lat", "location.lon", "location.fix", "location.alt_m", "location.speed"], g => {
    const lat = g<number | null>("location.lat") ?? null, lon = g<number | null>("location.lon") ?? null, alt = g<number | null>("location.alt_m");
    return { lat, lon, fix: !!g("location.fix"),
      posx: [alt != null ? `${Math.round(alt * 3.281).toLocaleString()} ft` : null, lat == null ? null : g("location.speed") ? "moving" : "parked"].filter(Boolean).join(" · ") };
  });
  return (
    <span className="muted" id="wx-sub">
      {!ready ? null : v.lat == null || v.lon == null ? "No GPS position yet"
        : <><span>{`Van ${fmtLL(v.lat, v.lon)}${v.fix ? "" : " · last known"}`}</span>{v.posx && <small>{v.posx}</small>}</>}
    </span>
  );
});

// ── forecast ──
const Forecast = memo(function Forecast({ target }: { target: LatLon | null }) {
  const { fc, err } = useForecast();
  const ready = useReady();
  const minute = useMinute();
  const lat = target?.[0], lon = target?.[1];
  useEffect(() => { if (lat != null && lon != null) void loadForecast(lat, lon); }, [lat, lon, minute]);   // refetch when due
  if (!fc) {
    // skeleton rows while it loads (app.js: "Loading forecast…"); without any position there is nothing to load
    return (
      <div className="wx-fc" id="wx-fc" aria-busy={!err}>
        {err ? <div className="wx-fc-msg">Forecast unavailable<br /><small>{err}</small></div>
          : ready && !target ? <div className="wx-fc-msg">Forecast: waiting for a position<br /><small>or tap the map</small></div>
          : Array.from({ length: 7 }, (_, i) => <div key={i} className="fc-row fc-skel" aria-hidden="true"><i /><i /></div>)}
      </div>
    );
  }
  return (
    <div className="wx-fc" id="wx-fc">
      {forecastRows(fc).map(r => (
        <div className="fc-row" title={r.label} key={r.key}>
          <div className="fc-day"><b>{r.day}</b><small>{r.label}</small></div>
          <div className="fc-ic"><Icon name={r.icon} /></div>
          <div className="fc-t"><b>{`${r.hi}°`}</b><span>{`${r.lo}°`}</span></div>
          <div className="fc-x">{r.pp != null && <span className="fc-pp"><Icon name="drop" />{`${r.pp}%`}</span>}<span>{`${r.wind} mph`}</span></div>
        </div>
      ))}
    </div>
  );
});

/** The forecast's target: the tapped spot, else the van (rounded to ~100 m so GPS jitter doesn't re-render). */
const ForecastFor = memo(function ForecastFor({ pick }: { pick: LatLon | null }) {
  const van = useSelect(LOC, g => {
    const lat = g<number | null>("location.lat"), lon = g<number | null>("location.lon");
    return lat == null || lon == null ? null : [Math.round(lat * 1e3) / 1e3, Math.round(lon * 1e3) / 1e3] as const;
  });
  return <Forecast target={pick ?? van} />;
});

// ── map sync: van position and traffic state into the controller ──
const VanSync = memo(function VanSync({ ctl }: { ctl: WxMap }) {
  const ll = useSelect(LOC, g => [g<number | null>("location.lat") ?? null, g<number | null>("location.lon") ?? null] as const);
  useEffect(() => { ctl.setVan(ll[0], ll[1]); }, [ctl, ll]);
  return null;
});

const TrafficBtn = memo(function TrafficBtn({ ctl, dark }: { ctl: WxMap | null; dark: boolean }) {
  const avail = useKey("traffic.available") === true;
  const [on, setOn] = useState(wxPrefs.traffic);
  useEffect(() => { ctl?.setTraffic(avail, on, dark); }, [ctl, avail, on, dark]);
  return (
    <button className={`wx-tog${on ? " on" : ""}`} id="wx-traffic" hidden={!avail}
      title={on ? "Showing slowdowns and incidents (TomTom) — tap to hide" : "Traffic hidden — tap to show"}
      onClick={() => { prefs.set("vp.traffic", on ? "off" : "on"); setOn(!on); }}>Traffic</button>
  );
});

// ── the map and its controls ──
const BASES: [WxBase, string][] = [["streets", "Streets"], ["sat", "Satellite"], ["topo", "Topo"]];
const LAYERS: [WxLayer, string][] = [["radar", "Radar"], ["ir", "IR clouds"], ["off", "Off"]];

const MapArea = memo(function MapArea({ dark, pick, onPick }: { dark: boolean; pick: LatLon | null; onPick: (p: LatLon | null) => void }) {
  const ready = useReady();
  const hasFix = useSelect(LOC, g => g("location.lat") != null && g("location.lon") != null);
  const refs = {
    map: useRef<HTMLDivElement>(null), tl: useRef<HTMLDivElement>(null), track: useRef<HTMLDivElement>(null),
    rail: useRef<HTMLDivElement>(null), fill: useRef<HTMLDivElement>(null), thumb: useRef<HTMLDivElement>(null),
    ticks: useRef<HTMLDivElement>(null), cast: useRef<HTMLDivElement>(null), rec: useRef<HTMLDivElement>(null),
    time: useRef<HTMLSpanElement>(null), play: useRef<HTMLButtonElement>(null), opac: useRef<HTMLDivElement>(null),
    opacTrack: useRef<HTMLDivElement>(null), opacFill: useRef<HTMLDivElement>(null), opacThumb: useRef<HTMLDivElement>(null),
  };
  const ctlRef = useRef<WxMap | null>(null);
  const [ctl, setCtl] = useState<WxMap | null>(null);
  const [zoom, setZoom] = useState({ in: true, out: true });
  const pickRef = useRef(onPick);
  pickRef.current = onPick;

  useEffect(() => {
    // the Leaflet map is built after the page's first frame is on screen (React runs a tap's effects before the
    // browser paints); coming back to a page whose map exists resumes it at once
    let mine: WxMap | null = null, cancel = () => {};
    const start = () => {
      let c = ctlRef.current;
      if (!c || c.destroyed) {
        const els = Object.fromEntries(Object.entries(refs).map(([k, r]) => [k, r.current])) as unknown as WxEls;
        c = ctlRef.current = new WxMap(els, {
          pick: ll => pickRef.current(ll),
          zoom: (i, o) => setZoom(z => (z.in === i && z.out === o ? z : { in: i, out: o })),
        });
      }
      c.resume();
      setCtl(c);
      mine = c;
    };
    if (ctlRef.current && !ctlRef.current.destroyed) start(); else cancel = afterPaint(start);
    const el = refs.map.current!;
    return () => {
      cancel();
      const m = mine;
      if (!m) return;
      m.pause();
      // hidden (<Activity>) keeps the DOM connected and the map with it; a real unmount removes the map
      setTimeout(() => { if (!el.isConnected && !m.destroyed) { m.destroy(); if (ctlRef.current === m) ctlRef.current = null; } }, 0);
    };
  }, []);   // eslint-disable-line react-hooks/exhaustive-deps

  // layers menu (☰): basemap, weather layer, overlays — all per screen
  const [menu, setMenu] = useState(false);
  const [base, setBase] = useState(wxPrefs.base);
  const [layer, setLayer] = useState(wxPrefs.layer);
  const [lands, setLands] = useState(wxPrefs.lands);
  const [span, setSpan] = useState(wxPrefs.span);
  const [speed, setSpeed] = useState(wxPrefs.speed);
  useEffect(() => {                                            // a tap outside closes it
    if (!menu) return;
    const down = (e: PointerEvent) => { if (!(e.target as Element | null)?.closest?.("#wx-menu-pop, #wx-menu")) setMenu(false); };
    document.addEventListener("pointerdown", down);
    return () => document.removeEventListener("pointerdown", down);
  }, [menu]);
  const off = layer === "off";

  const pickBase = (b: WxBase) => { prefs.set("vp.wx.base", b); setBase(b); ctl?.applyBase(); };
  const pickLayer = (l: WxLayer) => { if (l === layer) return; prefs.set("vp.wx.layer", l); setLayer(l); ctl?.relayer(); };
  const toggleLands = () => { prefs.set("vp.wx.lands", lands ? "off" : "on"); setLands(!lands); ctl?.applyLands(); };
  const nextSpan = () => {
    const v = RV_SPANS[(RV_SPANS.indexOf(span as 1 | 2 | 6) + 1) % RV_SPANS.length]!;
    prefs.set("vp.radar.span", String(v)); setSpan(v); void ctl?.respan();
  };
  const nextSpeed = () => {
    const v = RV_SPEEDS[(RV_SPEEDS.indexOf(speed as 1 | 2 | 4) + 1) % RV_SPEEDS.length]!;
    prefs.set("vp.radar.speed", String(v)); setSpeed(v); ctl?.respeed();
  };
  const recenter = () => ctl?.recenter(store.get<number>("location.lat") ?? null, store.get<number>("location.lon") ?? null);
  const pe = (f: (c: WxMap, e: PointerEvent) => void) => (e: RPointerEvent) => { if (ctl) f(ctl, e.nativeEvent); };

  return (
    <div className={`wx-wrap${dark ? " dark-map" : ""}`}>
      <div id="wx-map" ref={refs.map} />
      {ctl && <VanSync ctl={ctl} />}
      <button className={`wx-menu-btn${menu ? " on" : ""}`} id="wx-menu" aria-label="Map layers" aria-expanded={menu} title="Map layers"
        onClick={e => { e.stopPropagation(); setMenu(!menu); }}>
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M4 12h16M4 17h16" /></svg></button>
      <div className="wx-menu" id="wx-menu-pop" hidden={!menu}>
        <div className="wx-menu-h">Map</div>
        <div className="wx-seg" data-group="base">
          {BASES.map(([v, t]) => <button key={v} data-v={v} className={base === v ? "on" : undefined} onClick={() => pickBase(v)}>{t}</button>)}</div>
        <div className="wx-menu-h">Weather</div>
        <div className="wx-seg" data-group="wx">
          {LAYERS.map(([v, t]) => <button key={v} data-v={v} className={layer === v ? "on" : undefined} onClick={() => pickLayer(v)}>{t}</button>)}</div>
        <div className="wx-menu-h">Overlays</div>
        <div className="wx-togs">
          <button className={`wx-tog${lands ? " on" : ""}`} id="wx-lands" onClick={toggleLands}>Public lands</button>
          <TrafficBtn ctl={ctl} dark={dark} />
        </div>
      </div>
      <div className="wx-opac" id="wx-opac" ref={refs.opac} role="slider" aria-label="Radar opacity" aria-valuemin={10} aria-valuemax={100} hidden={off}
        onPointerDown={pe((c, e) => c.opacDown(e))} onPointerMove={pe((c, e) => c.opacMove(e))}
        onPointerUp={pe((c, e) => c.dragEnd(e))} onPointerCancel={pe((c, e) => c.dragEnd(e))}>
        <svg className="wx-opac-ico" viewBox="0 0 24 24" aria-hidden="true"><defs><clipPath id="wx-opac-half"><rect x="0" y="13" width="24" height="11" /></clipPath></defs>
          <path className="drop" d="M12 3.2c3.3 4.2 6 7.6 6 10.8a6 6 0 0 1-12 0c0-3.2 2.7-6.6 6-10.8z" />
          <path className="fill" clipPath="url(#wx-opac-half)" d="M12 3.2c3.3 4.2 6 7.6 6 10.8a6 6 0 0 1-12 0c0-3.2 2.7-6.6 6-10.8z" /></svg>
        <div className="wx-opac-track" id="wx-opac-track" ref={refs.opacTrack}>
          <div className="wx-opac-fill" id="wx-opac-fill" ref={refs.opacFill} /><div className="wx-opac-thumb" id="wx-opac-thumb" ref={refs.opacThumb} /></div>
      </div>
      <div className="wx-zoom" id="wx-zoom">
        <button id="wx-zin" aria-label="Zoom in" disabled={!zoom.in} onClick={() => ctl?.zoomIn()}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg></button>
        <button id="wx-zout" aria-label="Zoom out" disabled={!zoom.out} onClick={() => ctl?.zoomOut()}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12h14" /></svg></button></div>
      <button className="wx-locate" id="wx-recenter" aria-label="Re-centre on the van" title="Re-centre on the van" onClick={recenter}>
        <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="7" /><circle cx="12" cy="12" r="3" className="dot" />
          <path d="M12 2v3M12 19v3M2 12h3M19 12h3" /></svg></button>
      <button className="wx-speed wx-span" id="wx-span" aria-label="Loop length" hidden={off} title={`Loop: last ${span} h — tap to change`} onClick={nextSpan}>{span}h</button>
      <button className="wx-speed" id="wx-speed" aria-label="Radar loop speed" hidden={off} title={`Loop speed ${speed}× — tap to change`} onClick={nextSpeed}>{speed}×</button>
      <div className="wx-tl" id="wx-tl" ref={refs.tl} hidden={off}>
        <button className="wx-tl-play" id="wx-play" ref={refs.play} aria-label="Play radar loop" onClick={() => ctl?.togglePlay()}>
          <svg viewBox="0 0 24 24" aria-hidden="true"><path className="i-play" d="M8 5.5v13l10.5-6.5z" /><path className="i-pause" d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z" /></svg></button>
        <div className="wx-tl-track" id="wx-tl-track" ref={refs.track} role="slider" aria-label="Radar time"
          onPointerDown={pe((c, e) => c.trackDown(e))} onPointerMove={pe((c, e) => c.trackMove(e))}
          onPointerUp={pe((c, e) => c.dragEnd(e))} onPointerCancel={pe((c, e) => c.dragEnd(e))}>
          <div className="wx-tl-rail" ref={refs.rail}>
            <div className="wx-tl-rec" id="wx-tl-rec" ref={refs.rec} /><div className="wx-tl-cast" id="wx-tl-cast" ref={refs.cast} />
            <div className="wx-tl-ticks" id="wx-tl-ticks" ref={refs.ticks} /><div className="wx-tl-fill" id="wx-tl-fill" ref={refs.fill} />
            <div className="wx-tl-thumb" id="wx-tl-thumb" ref={refs.thumb} /></div></div>
        <span className="wx-tl-time" id="wx-time" ref={refs.time}>—</span>
      </div>
      <div className="wx-hint" id="wx-hint" hidden={!!pick}>Tap the map for that spot's forecast</div>
      <div className="wx-empty" id="wx-empty" hidden={!ready || hasFix}>Waiting for a GPS fix from the Peplink…</div>
    </div>
  );
});

// ── page ──
const MapModeBtn = memo(function MapModeBtn({ mode, dark, onCycle }: { mode: MapMode; dark: boolean; onCycle: () => void }) {
  return (
    <button className={`ctl-pill wx-btn${mode !== "auto" ? " on" : ""}`} id="wx-dark" title={mapModeTitle(mode, dark)} onClick={onCycle}>
      Map: {mode[0]!.toUpperCase()}{mode.slice(1)}
    </button>
  );
});

const readMode = (): MapMode => { const m = prefs.get("vp.wxmap.mode", "auto") as MapMode; return WX_MODES.includes(m) ? m : "auto"; };

export default function WeatherPage() {
  const [mode, setMode] = useState<MapMode>(readMode);
  const [pick, setPick] = useState<LatLon | null>(() => P.pick);
  const minute = useMinute();                                    // sunrise / sunset crossings while on screen
  const dark = useSelect([...LOC, K.night], g => darkMapOn(mode, wxSun(g<number>("location.lat"), g<number>("location.lon"), minute), g(K.night) === true));
  const cycle = () => {
    const m = WX_MODES[(WX_MODES.indexOf(mode) + 1) % WX_MODES.length]!;
    prefs.set("vp.wxmap.mode", m); setMode(m);
  };
  const onPick = useCallback((ll: LatLon | null) => { resetForecast(); setPick(ll); }, []);
  return (
    <div className="card wx-card">
      <div className="card-h">
        <h2>Weather radar</h2><WxLoc /><WxSub />
        <MapModeBtn mode={mode} dark={dark} onCycle={cycle} />
      </div>
      {/* AetherMap's dark basemap (AetherSDR BasemapStyle::darkImage): reversed luminance ramped from
          color.map.darkDetail (#c8d8e8) to color.map.darkBackground (#18212b); radar is not filtered. */}
      <svg width="0" height="0" style={{ position: "absolute" }} aria-hidden="true"><filter id="wx-dark-filter" colorInterpolationFilters="sRGB">
        <feColorMatrix type="matrix" values="-0.14674 -0.49363 -0.04983 0 0.78431  -0.15257 -0.51326 -0.05181 0 0.84706  -0.15757 -0.53009 -0.05351 0 0.90980  0 0 0 1 0" /></filter></svg>
      <div className="wx-body">
        <MapArea dark={dark} pick={pick} onPick={onPick} />
        <ForecastFor pick={pick} />
      </div>
    </div>
  );
}
