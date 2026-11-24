// The Weather map, imperative (ported from app.js initWxMap / loadRadar / rvSet / rvPlay / applyTraffic …): a
// Leaflet map with an OpenStreetMap / USGS / OpenTopoMap basemap, the BLM public-lands overlay, the radar loop
// (RadarGL, or tile layers without WebGL2) and TomTom traffic, centred on the van.
//
// Lifetime (React <Activity>): the Leaflet map itself is kept while the page is hidden (pan, zoom and the pinned
// spot survive, and coming back shows the tiles at once); everything that runs or holds GPU memory — the loop,
// the timers, the radar's WebGL context and textures — is torn down by pause() and rebuilt by resume(). destroy()
// (a real unmount) removes the map; its view, the pin and the frame list live on in module state (P).
import * as L from "leaflet";
import { prefs } from "../../lib/prefs";
import { buildFrames, frameLabel, frameMs, keepPosition, LIBREWXR_INDEX, nowIndex, RV_SPANS, RV_SPEEDS,
  type Frame, type Recorded, type WeatherMaps, type WxLayer } from "./frames";
import { kmBetween, type LatLon } from "./geo";
import { RadarGL } from "./radarGL";

// ── per-screen preferences (same keys as app.js) ──
export const WX_BASES = {
  streets: ["https://tile.openstreetmap.org/{z}/{x}/{y}.png", { attribution: "© OpenStreetMap contributors" }],
  sat: ["https://basemap.nationalmap.gov/arcgis/rest/services/USGSImageryOnly/MapServer/tile/{z}/{y}/{x}",
    { attribution: "Imagery © USGS The National Map", className: "wx-base sat" }],
  topo: ["https://{s}.tile.opentopomap.org/{z}/{x}/{y}.png", { attribution: "© OpenTopoMap (CC-BY-SA) · © OpenStreetMap contributors", subdomains: "abc" }],
} as const satisfies Record<string, readonly [string, L.TileLayerOptions]>;
export type WxBase = keyof typeof WX_BASES;
export const LANDS_URL = "https://gis.blm.gov/arcgis/rest/services/lands/BLM_Natl_SMA_Cached_without_PriUnk/MapServer/tile/{z}/{y}/{x}";

const pick = <T extends string>(k: string, opts: readonly T[], d: T): T => { const v = prefs.get(k, d) as T; return opts.includes(v) ? v : d; };
export const wxPrefs = {
  base: () => pick<WxBase>("vp.wx.base", Object.keys(WX_BASES) as WxBase[], "streets"),
  layer: () => pick<WxLayer>("vp.wx.layer", ["radar", "ir", "off"], "radar"),
  lands: () => prefs.get("vp.wx.lands", "off") === "on",
  traffic: () => prefs.get("vp.traffic", "on") === "on",
  span: () => { const v = +prefs.get("vp.radar.span", "2"); return (RV_SPANS as readonly number[]).includes(v) ? v : 2; },
  speed: () => { const v = +prefs.get("vp.radar.speed", "1"); return (RV_SPEEDS as readonly number[]).includes(v) ? v : 1; },
  alpha: () => { const v = +prefs.get("vp.radar.opacity", ".85"); return isFinite(v) ? Math.max(.1, Math.min(1, v)) : .85; },
};

// ── state that outlives the page's mount (module level) ──
export const P = {
  view: null as { c: LatLon; z: number } | null,   // the map's view when it was last removed
  center: null as LatLon | null,                    // where it auto-centred on the van (re-centres after a > 15 km move)
  pick: null as LatLon | null,                      // the tapped spot (forecast for it)
  frames: [] as Frame[], flows: [] as number[], now: -1, at: 0, pos: 0, layer: "" as WxLayer | "",
};

export interface WxEls {
  map: HTMLDivElement; tl: HTMLDivElement; track: HTMLDivElement; rail: HTMLDivElement; fill: HTMLDivElement;
  thumb: HTMLDivElement; ticks: HTMLDivElement; cast: HTMLDivElement; rec: HTMLDivElement; time: HTMLSpanElement;
  play: HTMLButtonElement; opac: HTMLDivElement; opacTrack: HTMLDivElement; opacFill: HTMLDivElement; opacThumb: HTMLDivElement;
}
export interface WxEvents { pick(ll: LatLon | null): void; zoom(canIn: boolean, canOut: boolean): void }

/** One line to the backend log (kiosk only) so the kiosk's GPU and frame rate can be read from the Pi (app.js). */
const infoSent = new Set<string>();
function clientInfo(kind: string, data: object): void {
  if (infoSent.has(kind)) return;
  infoSent.add(kind);
  fetch("/api/client-info", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ kind, ...data }) }).catch(() => {});
}

const setText = (el: Element, s: string) => { if (el.textContent !== s) el.textContent = s; };

/** The radar loop's frame cap (app.js: 15 fps). 60 = every frame of the van's 60 Hz panel; lower it to save GPU
 *  power if the bench shows it matters (the in-between images are interpolated either way). */
export const RADAR_MAX_FPS = 60;

export class WxMap {
  readonly map: L.Map;
  private active = false;
  private dead = false;
  private gl: RadarGL | null = null;
  private tiles: (L.TileLayer | null)[] = [];     // tile-layer fallback (no WebGL2): one layer per frame
  private idx = -1;
  private timer: ReturnType<typeof setInterval> | null = null;   // fallback loop
  private raf = 0;
  private minute: ReturnType<typeof setInterval> | null = null;
  private baseLayer: L.TileLayer | null = null;
  private baseName = "";
  private lands: L.TileLayer | null = null;
  private traffic: L.TileLayer[] | null = null;
  private trafficWant: { avail: boolean; on: boolean; dark: boolean } = { avail: false, on: false, dark: false };
  private van: L.Marker | null = null;
  private pin: L.Marker | null = null;
  private alpha = wxPrefs.alpha();
  private drag: { id: number; el: HTMLElement } | null = null;
  private stats: { t0: number; frames: number; ms: number } | null = null;
  private loadSeq = 0;
  private started = false;                        // the deferred part of resume() ran (GL, frames, layers)
  private later: (() => void) | null = null;

  constructor(private readonly els: WxEls, private readonly ev: WxEvents) {
    const v = P.view;
    const map = this.map = L.map(els.map, { zoomControl: false, attributionControl: true, zoomSnap: 0.5 })
      .setView(v ? [v.c[0], v.c[1]] : [45, -110], v ? v.z : 7);
    map.attributionControl.addAttribution("Weather © LibreWXR");
    map.createPane("lands").style.zIndex = "220";                 // public lands: over the basemap, under the radar
    map.createPane("radar").style.zIndex = "250";                 // above the basemap, below traffic (300) and markers
    map.createPane("traffic").style.zIndex = "300";
    this.applyBase();
    this.applyLands();
    map.on("click", e => this.setPick([e.latlng.lat, e.latlng.lng]));
    map.on("zoomend", () => this.zoomState());
    if (P.pick) this.drawPin(P.pick);
    this.zoomState();
    this.paintAlpha();
  }

  get destroyed() { return this.dead; }

  // ── lifetime ──
  /**
   * The page is visible: size, radar, timers. The timeline is painted at once; the WebGL context, its shaders and
   * the textures are set up after the page's first frame is on screen (React runs a tap's effects before the
   * browser paints, so doing it here would hold the page switch for the context + shader compile).
   */
  resume(): void {
    if (this.dead || this.active) return;
    this.active = true;
    this.paintTimeline();
    if (P.frames.length && P.layer === wxPrefs.layer()) this.set(P.pos);
    this.later = afterPaint(() => {
      this.later = null;
      if (!this.active) return;
      this.started = true;
      this.map.invalidateSize();
      this.initGL();
      if (this.gl && wxPrefs.layer() === "off") this.gl.canvas.hidden = true;
      if (P.frames.length && P.layer === wxPrefs.layer()) {
        this.attachFrames();
        this.set(P.pos);
      }
      void this.loadRadar(P.layer !== "" && P.layer !== wxPrefs.layer());
      this.applyTraffic();
    });
    this.minute = setInterval(() => {                              // what app.js re-did on every state update
      void this.loadRadar();                                       // every 10 min
      this.applyTraffic();                                         // 5-min tile buckets
      if (P.frames.length) this.paintLabel();                      // "· 7 min ago"
    }, 60e3);
  }

  /** The page is hidden: stop everything that runs; free the GPU. The map (DOM) stays. */
  pause(): void {
    if (!this.active) return;
    this.active = false;
    this.later?.(); this.later = null;
    this.started = false;
    this.stop();
    if (this.minute) clearInterval(this.minute);
    this.minute = null;
    this.endDrag();
    if (this.gl) { const g = this.gl; this.gl = null; try { this.map.removeLayer(g); } catch { g.dispose(); } }
    this.tiles.forEach(l => l && this.map.removeLayer(l));
    this.tiles = this.tiles.map(() => null);
    this.idx = -1;
  }

  /** Unmounted for good. */
  destroy(): void {
    if (this.dead) return;
    this.pause();
    const c = this.map.getCenter();
    P.view = { c: [c.lat, c.lng], z: this.map.getZoom() };
    this.dead = true;
    this.map.remove();
  }

  // ── layers ──
  applyBase(): void {
    const want = wxPrefs.base();
    if (this.dead || this.baseName === want) return;
    const [url, o] = WX_BASES[this.baseName = want];
    if (this.baseLayer) this.map.removeLayer(this.baseLayer);
    this.baseLayer = L.tileLayer(url, { maxZoom: 12, className: "wx-base", ...o }).addTo(this.map);
  }

  /** BLM Surface Management Agency: who manages the land (BLM, Forest Service, NPS, state, tribal …). */
  applyLands(): void {
    if (this.dead) return;
    const on = wxPrefs.lands();
    if (on && !this.lands) this.lands = L.tileLayer(LANDS_URL, { pane: "lands", opacity: 0.45, maxZoom: 12, attribution: "Public lands © BLM" }).addTo(this.map);
    if (!on && this.lands) { this.map.removeLayer(this.lands); this.lands = null; }
  }

  /** TomTom traffic (proxied by the weather service so its key stays on the Pi): slow roads + incidents, above the
   *  radar so a storm doesn't hide a closure. The URL changes every 5 min (tile refresh). */
  setTraffic(avail: boolean, on: boolean, dark: boolean): void {
    this.trafficWant = { avail, on, dark };
    this.applyTraffic();
  }
  private applyTraffic(): void {
    if (this.dead) return;
    const { avail, on, dark } = this.trafficWant;
    if (!avail || !on) { if (this.traffic) { this.traffic.forEach(l => this.map.removeLayer(l)); this.traffic = null; } return; }
    if (!this.active && this.traffic) return;                     // don't refetch for a hidden map
    const q = `?b=${Math.floor(Date.now() / 300e3)}`, inc = dark ? "s0-dark" : "s0";
    const urls = [`/api/traffic/flow/major/{z}/{x}/{y}.png${q}`, `/api/traffic/incidents/${inc}/{z}/{x}/{y}.png${q}`];
    // 512 px tiles: map zoom N → tile zoom N-1 (like the radar)
    if (!this.traffic) this.traffic = urls.map((u, i) => L.tileLayer(u, { tileSize: 512, zoomOffset: -1, minZoom: 3, maxZoom: 12, pane: "traffic",
      zIndex: 7 + i, className: "wx-traffic", attribution: i ? "Traffic © TomTom" : "" }).addTo(this.map));
    else this.traffic.forEach((l, i) => { if ((l as unknown as { _url: string })._url !== urls[i]) l.setUrl(urls[i]!); });
  }

  // ── van, pin, view ──
  /** The van's position: marker, and re-centre when it moved > 15 km from where the map last centred on it. */
  setVan(lat: number | null, lon: number | null): void {
    if (this.dead || lat == null || lon == null) return;
    const icon = L.divIcon({ className: "wx-van", html: "<i></i>", iconSize: [22, 22], iconAnchor: [11, 11] });
    if (this.van) this.van.setLatLng([lat, lon]);
    else this.van = L.marker([lat, lon], { icon, interactive: false, zIndexOffset: 500, keyboard: false }).addTo(this.map);
    if (!P.center || kmBetween(P.center, [lat, lon]) > 15) { P.center = [lat, lon]; this.map.setView([lat, lon], 7); }
  }

  private drawPin(ll: LatLon): void {
    const icon = L.divIcon({ className: "wx-pin", html: "<i></i>", iconSize: [26, 26], iconAnchor: [13, 13] });
    if (this.pin) this.pin.setLatLng([ll[0], ll[1]]);
    else this.pin = L.marker([ll[0], ll[1]], { icon, interactive: false, keyboard: false }).addTo(this.map);
  }

  /** A tap on the map pins that spot (its forecast is shown). */
  private setPick(ll: LatLon): void {
    P.pick = ll;
    this.drawPin(ll);
    this.ev.pick(ll);
  }

  /** Re-centre: back to the van, its forecast, no pin. */
  recenter(lat: number | null, lon: number | null): void {
    P.center = null; P.pick = null;
    if (this.pin) { this.map.removeLayer(this.pin); this.pin = null; }
    this.ev.pick(null);
    this.setVan(lat, lon);
  }

  zoomIn(): void { if (!this.dead) this.map.zoomIn(); }
  zoomOut(): void { if (!this.dead) this.map.zoomOut(); }
  private zoomState(): void {
    const z = this.map.getZoom();
    this.ev.zoom(z < this.map.getMaxZoom(), z > this.map.getMinZoom());
  }

  // ── radar frames ──
  private initGL(): void {
    if (this.gl || noGL || new URLSearchParams(location.search).get("radar") === "tiles") return;
    const host = {
      frames: () => P.frames, flowTimes: () => P.flows, alpha: () => this.alpha, pos: () => P.pos,
      playing: () => !!this.raf, lost: (why: string) => this.fallback(why),
    };
    const g = new RadarGL(host);
    try { g.addTo(this.map); this.gl = g; clientInfo("radar-gl", { renderer: g.renderer }); }
    catch (e) { try { this.map.removeLayer(g); } catch { /* never added */ } g.dispose(); clientInfo("radar-gl", { error: String(e) }); }
  }

  /** WebGL failed mid-session: back to tile layers for the rest of the session. */
  private fallback(why: string): void {
    clientInfo("radar-gl", { fallback: why });
    this.stop();
    if (this.gl) { const g = this.gl; this.gl = null; try { this.map.removeLayer(g); } catch { g.dispose(); } }
    noGL = true;
    this.set(Math.round(P.pos));
  }

  /**
   * Frames: LibreWXR's index (RainViewer-compatible API, crisper and deeper zoom, as used by AetherMap) plus the
   * frames the Pi recorded. Refreshed every 10 min (a failure retries after 1 min).
   */
  async loadRadar(force = false): Promise<void> {
    const layer = wxPrefs.layer();
    if (!force && Date.now() - P.at < 10 * 60e3 && (P.frames.length || layer === "off")) return;
    P.at = Date.now();
    const seq = ++this.loadSeq;
    if (layer === "off") { this.clear(); P.layer = "off"; return; }
    try {
      const [j, rec] = await Promise.all([
        fetch(LIBREWXR_INDEX).then(r => r.json() as Promise<WeatherMaps>),
        fetch("/api/radar/recorded").then(r => r.json() as Promise<Recorded>).catch(() => ({ frames: [], flows: [] }) as Recorded)]);
      if (this.dead || seq !== this.loadSeq || wxPrefs.layer() !== layer) return;   // switched again while loading
      const frames = buildFrames(j, rec, layer, wxPrefs.span());
      if (!frames.length) throw new Error("no frames");
      const prev = P.frames[Math.round(P.pos)], wasNow = !prev || Math.round(P.pos) === P.now;
      P.frames = frames; P.flows = layer === "ir" ? [] : rec.flows || []; P.now = nowIndex(frames); P.layer = layer;
      P.pos = keepPosition(frames, prev?.time ?? null, wasNow, force);
      if (!this.active || !this.started) return;
      this.attachFrames();
      this.set(P.pos);
    } catch {
      P.at = Date.now() - 9 * 60e3;
      if (this.active && seq === this.loadSeq) setText(this.els.time, "radar unavailable");
    }
  }

  /** The frame list changed (or the page came back): ticks, the GL's textures, the fallback's layers. */
  private attachFrames(): void {
    this.tiles.forEach(l => l && this.map.removeLayer(l));
    this.tiles = P.frames.map(() => null); this.idx = -1;
    if (this.gl) { this.gl.canvas.hidden = false; this.gl.setFrames(P.frames); }
    this.paintTimeline();
  }

  /** Weather layer off. */
  private clear(): void {
    this.stop();
    this.tiles.forEach(l => l && this.map.removeLayer(l));
    P.frames = []; P.flows = []; P.now = -1; P.pos = 0; this.tiles = []; this.idx = -1;
    if (this.gl) { this.gl.setFrames([]); this.gl.canvas.hidden = true; }
    this.paintTimeline();
  }

  private paintTimeline(): void {
    const n = P.frames.length, pc = (i: number) => (n > 1 ? i / (n - 1) * 100 : 100), e = this.els;
    e.ticks.innerHTML = P.frames.map((f, i) => `<i class="${f.kind}${i === P.now ? " now" : ""}" style="left:${pc(i)}%"></i>`).join("");
    e.cast.style.left = `${n ? pc(P.now) : 100}%`;
    const firstLive = P.frames.findIndex(f => f.kind !== "rec");
    e.rec.style.width = `${n ? pc(firstLive < 0 ? n - 1 : firstLive) : 0}%`;
    if (!n) { e.fill.style.width = "0%"; e.thumb.style.left = "0%"; setText(e.time, "—"); e.tl.classList.remove("cast"); }
  }

  private paintLabel(): void {
    const f = P.frames[Math.round(P.pos)];
    if (f) setText(this.els.time, frameLabel(f));
  }

  /** Move the timeline to a (fractional, with WebGL) frame position: label, rail, and the radar itself. */
  set(pos: number): void {
    const n = P.frames.length;
    if (!n || this.dead) return;
    P.pos = Math.max(0, Math.min(n - 1, pos));
    const i = Math.round(P.pos), f = P.frames[i]!, e = this.els;
    setText(e.time, frameLabel(f));
    e.tl.classList.toggle("cast", f.kind === "cast");
    const p = n > 1 ? P.pos / (n - 1) * 100 : 100, pNow = n > 1 ? P.now / (n - 1) * 100 : 100;
    e.fill.style.width = `${Math.min(p, pNow)}%`;                 // the fill stops at "now"
    e.thumb.style.left = `${p}%`;
    e.track.setAttribute("aria-valuenow", String(i));
    if (!this.active || !this.started) return;
    if (this.gl) { this.gl.want(P.pos); this.gl.draw(P.pos); return; }
    // tile-layer fallback: the shown frame plus one behind and three ahead attached (prefetch); the rest detached
    const keep = new Set((this.timer ? [-1, 0, 1, 2, 3] : [0]).map(d => (i + d + n) % n));
    this.tiles.forEach((l, k) => { if (l && !keep.has(k) && this.map.hasLayer(l)) this.map.removeLayer(l); });
    keep.forEach(k => { const l = this.tileLayer(k); if (!this.map.hasLayer(l)) l.setOpacity(0).addTo(this.map); });
    if (this.idx >= 0 && this.tiles[this.idx] && this.idx !== i) this.tiles[this.idx]!.setOpacity(0);
    this.idx = i;
    this.tiles[i]!.setOpacity(this.alpha);
  }

  private tileLayer(i: number): L.TileLayer {
    const f = P.frames[i]!;
    // Leaflet scales tiles outside the frame's zoom range (native zoom is in map zooms = tile zoom + 1)
    return this.tiles[i] ??= L.tileLayer(f.url, { opacity: 0, tileSize: 512, zoomOffset: -1, maxZoom: 12, zIndex: 5, className: "wx-radar",
      minNativeZoom: f.minz + 1, maxNativeZoom: f.maxz + 1 });
  }

  // ── the loop ──
  get playing(): boolean { return !!(this.timer || this.raf); }

  stop(): void {
    if (this.timer) { clearInterval(this.timer); this.timer = null; }
    if (this.raf) { cancelAnimationFrame(this.raf); this.raf = 0; }
    this.els.tl.classList.remove("playing");
    this.els.play.setAttribute("aria-label", "Play radar loop");
  }

  /**
   * Play: with WebGL the loop draws up to RADAR_MAX_FPS (app.js capped it at 15 fps) with motion-interpolated
   * in-between images; without, it steps frame by frame. Starts at the beginning of the period when at "now".
   */
  play(): void {
    if (!P.frames.length || !this.active || !this.started) return;
    this.els.tl.classList.add("playing");
    this.els.play.setAttribute("aria-label", "Pause radar loop");
    if (P.pos >= P.now) this.set(0);
    if (!this.gl) {
      this.timer = setInterval(() => this.set((Math.round(P.pos) + 1) % P.frames.length), frameMs(wxPrefs.speed()));
      this.set(P.pos);
      return;
    }
    let last = performance.now();
    this.stats = { t0: last, frames: 0, ms: 0 };
    const tick = (now: number) => {
      this.raf = requestAnimationFrame(tick);
      if (!this.gl || !this.active) { this.stop(); return; }
      if (now - last < 1000 / RADAR_MAX_FPS - 2) return;
      const dt = Math.min(100, now - last);                        // a stalled frame doesn't jump the loop
      last = now;
      const n = P.frames.length;
      let p = P.pos + dt / frameMs(wxPrefs.speed());
      if (p > n - 1) p = 0;
      if (!this.gl.ready(p)) { this.gl.want(p); return; }          // hold until the pair's textures are in
      const t0 = performance.now();
      this.set(p);
      const s = this.stats!;
      s.frames++; s.ms += performance.now() - t0;
      if (now - s.t0 > 10e3 && !infoSent.has("radar-fps"))
        clientInfo("radar-fps", { fps: +(s.frames / ((now - s.t0) / 1000)).toFixed(1), ms_per_frame: +(s.ms / s.frames).toFixed(2) });
    };
    this.raf = requestAnimationFrame(tick);
  }

  togglePlay(): void {
    if (this.playing) { this.stop(); this.set(Math.round(P.pos)); return; }   // pause on a real frame
    this.play();
  }

  /** Loop length changed: reload the frames, keep playing if it was. */
  async respan(): Promise<void> {
    const playing = this.playing;
    this.stop();
    P.at = 0;
    await this.loadRadar(true);
    if (playing && P.frames.length && this.active) this.play();
  }

  /** Loop speed changed (the WebGL loop reads it every frame). */
  respeed(): void {
    if (this.timer) { clearInterval(this.timer); this.timer = setInterval(() => this.set((Math.round(P.pos) + 1) % P.frames.length), frameMs(wxPrefs.speed())); }
  }

  /** Weather layer changed (radar / IR / off). */
  relayer(): void {
    this.stop();
    P.at = 0;
    void this.loadRadar(true);
  }

  // ── timeline scrub (drag or tap the rail; continuous with WebGL, snapped to frames without) ──
  private trackAt(e: PointerEvent): number {
    const r = this.els.rail.getBoundingClientRect();
    const p = Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)) * (P.frames.length - 1);
    return this.gl ? p : Math.round(p);
  }
  trackDown(e: PointerEvent): void {
    if (!P.frames.length) return;
    this.stop();
    this.drag = { id: e.pointerId, el: this.els.track };
    try { this.els.track.setPointerCapture(e.pointerId); } catch { /* synthetic */ }
    this.els.track.classList.add("drag");
    this.set(this.trackAt(e));
  }
  trackMove(e: PointerEvent): void {
    if (this.drag?.el !== this.els.track || this.drag.id !== e.pointerId) return;
    const p = this.trackAt(e);
    if (p !== P.pos) this.set(p);
  }

  // ── radar opacity: the glass slider above the zoom buttons (10–100 %, 5 % steps), remembered per screen ──
  private paintAlpha(): void {
    const p = (this.alpha - .1) / .9 * 100, e = this.els;
    e.opacFill.style.height = `${p}%`; e.opacThumb.style.bottom = `${p}%`;
    e.opac.setAttribute("aria-valuenow", String(Math.round(this.alpha * 100)));
    e.opac.title = `Radar opacity ${Math.round(this.alpha * 100)} %`;
  }
  setAlpha(v: number, save = true): void {
    this.alpha = Math.max(.1, Math.min(1, Math.round(v * 20) / 20));
    this.paintAlpha();
    if (save) prefs.set("vp.radar.opacity", String(this.alpha));
    if (this.gl) this.gl.draw(P.pos);
    else if (this.idx >= 0 && this.tiles[this.idx]) this.tiles[this.idx]!.setOpacity(this.alpha);
  }
  private opacAt(e: PointerEvent): number {
    const r = this.els.opacTrack.getBoundingClientRect();
    return .1 + .9 * Math.max(0, Math.min(1, (r.bottom - e.clientY) / r.height));
  }
  opacDown(e: PointerEvent): void {
    this.drag = { id: e.pointerId, el: this.els.opac };
    try { this.els.opac.setPointerCapture(e.pointerId); } catch { /* synthetic */ }
    this.els.opac.classList.add("drag");
    this.setAlpha(this.opacAt(e));
  }
  opacMove(e: PointerEvent): void {
    if (this.drag?.el === this.els.opac && this.drag.id === e.pointerId) this.setAlpha(this.opacAt(e));
  }

  /** pointerup / pointercancel on either slider. */
  dragEnd(e: PointerEvent): void { if (this.drag?.id === e.pointerId) this.endDrag(); }
  private endDrag(): void {
    if (!this.drag) return;
    this.drag.el.classList.remove("drag");
    this.drag = null;
  }

  /** Textures held by the radar (tests). */
  get glTextures(): number { return this.gl?.textures ?? 0; }
}

/** Run f once the current frame has been painted (a frame, then a task). Returns cancel. */
export function afterPaint(f: () => void): () => void {
  let t: ReturnType<typeof setTimeout> | null = null;
  const r = requestAnimationFrame(() => { t = setTimeout(f, 0); });
  return () => { cancelAnimationFrame(r); if (t) clearTimeout(t); };
}

/** Set when WebGL failed mid-session: later mounts go straight to tile layers. */
let noGL = false;
export const glDisabled = () => noGL;
