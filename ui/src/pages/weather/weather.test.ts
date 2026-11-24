// Weather page logic: sun times, distances, frame lists, labels, forecast rows (invented positions and frames).
import { afterEach, describe, expect, it, vi } from "vitest";
import { buildFrames, frameLabel, keepPosition, nearestFlow, nowIndex, type WeatherMaps } from "./frames";
import { _resetForecastModule, forecastNow, forecastQuery, forecastRows, loadForecast, resetForecast, WMO } from "./forecast";
import { darkMapOn, fmtLL, kmBetween, mapModeTitle, sunTimes, wxSun } from "./geo";

describe("geo", () => {
  it("sunTimes: rise before noon before set, ~9–15 h of daylight at mid latitudes", () => {
    const st = sunTimes(44.1, -103.5, new Date(2026, 10, 23))!;
    expect(st.rise).toBeLessThan(st.noon);
    expect(st.noon).toBeLessThan(st.set);
    const h = (st.set - st.rise) / 3.6e6;
    expect(h).toBeGreaterThan(8.5);
    expect(h).toBeLessThan(10.5);                  // late November
    const jun = sunTimes(44.1, -103.5, new Date(2026, 5, 21))!;
    expect((jun.set - jun.rise) / 3.6e6).toBeGreaterThan(15);
  });
  it("sunTimes: polar night / day → null", () => {
    expect(sunTimes(80, 10, new Date(2026, 11, 21))).toBeNull();
    expect(sunTimes(80, 10, new Date(2026, 5, 21))).toBeNull();
  });
  it("kmBetween / fmtLL", () => {
    expect(kmBetween([0, 0], [0, 1])).toBeCloseTo(111.19, 1);
    expect(kmBetween([44, -103], [44, -103])).toBe(0);
    expect(fmtLL(44.1234, -103.4567)).toBe("44.123° N, 103.457° W");
    expect(fmtLL(-33.9, 151.2)).toBe("33.900° S, 151.200° E");
  });
  it("wxSun / darkMapOn: Auto follows the sun at the van, else night mode", () => {
    const st = sunTimes(44.1, -103.5, new Date(2026, 10, 23, 12))!;
    expect(wxSun(44.1, -103.5, st.noon)!.dark).toBe(false);
    expect(wxSun(44.1, -103.5, st.set + 60e3)!.dark).toBe(true);
    expect(wxSun(44.1, -103.5, st.noon)!.next).toBe(st.set);
    expect(wxSun(null, null)).toBeNull();
    expect(darkMapOn("auto", { dark: true }, false)).toBe(true);
    expect(darkMapOn("auto", null, true)).toBe(true);
    expect(darkMapOn("auto", null, false)).toBe(false);
    expect(darkMapOn("dark", { dark: false }, false)).toBe(true);
    expect(darkMapOn("light", { dark: true }, true)).toBe(false);
    expect(mapModeTitle("auto", true)).toBe("Auto: dark (sunset to sunrise) — tap for Dark");
    expect(mapModeTitle("light", false)).toBe("Always light — tap for Auto");
  });
});

const T0 = 1_800_000_000;
const J: WeatherMaps = {
  host: "https://tiles.example.test", generated: T0 + 5,
  radar: { past: Array.from({ length: 13 }, (_, i) => ({ time: T0 - (12 - i) * 600, path: `/p${i}` })),
    nowcast: [{ time: T0 + 600, path: "/c0" }, { time: T0 + 1200, path: "/c1" }] },
  satellite: { infrared: [{ time: T0 - 7200, path: "/s0" }, { time: T0 - 3600, path: "/s1" }, { time: T0, path: "/s2" }] },
};
const REC = { frames: [T0 - 9000, T0 - 8400, T0 - 7800], flows: [T0 - 9000] };

describe("frames", () => {
  it("radar: recorded (older than the source) + past + nowcast, cut to the loop length", () => {
    const f2 = buildFrames(J, REC, "radar", 2);
    expect(f2.map(f => f.kind)).toEqual([...Array(13).fill("past"), "cast", "cast"]);
    const f6 = buildFrames(J, REC, "radar", 6);
    expect(f6.filter(f => f.kind === "rec").map(f => f.time)).toEqual(REC.frames);
    expect(f6[0]!.url).toBe(`/api/radar/tile/${T0 - 9000}/{z}/{x}/{y}.png`);
    expect(f6[0]!.minz).toBe(4);
    expect(f6.find(f => f.kind === "past")!.url).toBe("https://tiles.example.test/p0/512/{z}/{x}/{y}/6/0_0.png");
    expect(f6.at(-1)!.url).toBe(`https://tiles.example.test/c1/512/{z}/{x}/{y}/6/0_0.png?g=${T0 + 5}`);
    expect(nowIndex(f6)).toBe(3 + 12);
  });
  it("a recorded frame newer than the source's oldest is dropped (the source has it)", () => {
    const f = buildFrames(J, { frames: [T0 - 7200, T0 - 7300] }, "radar", 6);
    expect(f.filter(x => x.kind === "rec").map(x => x.time)).toEqual([T0 - 7300]);
  });
  it("IR: hourly satellite frames, scheme 0, now = the last one", () => {
    const f = buildFrames(J, REC, "ir", 1);
    expect(f.map(x => x.time)).toEqual([T0 - 3600, T0]);
    expect(f[0]!.url).toBe("https://tiles.example.test/s1/512/{z}/{x}/{y}/0/0_0.png");
    expect(nowIndex(f)).toBe(1);
  });
  it("labels", () => {
    const now = T0 * 1000;
    expect(frameLabel({ time: T0 - 420, kind: "past", minz: 0, maxz: 10, url: "" }, now)).toMatch(/ · 7 min ago$/);
    expect(frameLabel({ time: T0 - 5400, kind: "rec", minz: 4, maxz: 7, url: "" }, now)).toMatch(/ · 1 h 30 min ago$/);
    expect(frameLabel({ time: T0 + 1200, kind: "cast", minz: 0, maxz: 10, url: "" }, now)).toMatch(/ · forecast \+20 min$/);
    expect(frameLabel({ time: T0 - 3600, kind: "ir", minz: 0, maxz: 6, url: "" }, now)).toMatch(/^IR · .* · 1 h ago$/);
  });
  it("keepPosition: 'now' stays now; otherwise the same time survives a refresh", () => {
    const f = buildFrames(J, REC, "radar", 2);
    expect(keepPosition(f, null, true, false)).toBe(12);
    expect(keepPosition(f, T0 - 3000, false, false)).toBe(7);
    expect(keepPosition(f, T0 - 3000, false, true)).toBe(12);
    expect(keepPosition(f, T0 + 99999, false, false)).toBe(f.length - 1);
  });
  it("nearestFlow: the closest recorded pair within 3 h", () => {
    expect(nearestFlow([], T0)).toBeNull();
    expect(nearestFlow([T0 - 600, T0 - 1200], T0)).toBe(T0 - 600);
    expect(nearestFlow([T0 - 4 * 3600], T0)).toBeNull();
  });
});

describe("forecast", () => {
  it("query: rounded position, 7 days, °F or °C, mph", () => {
    const q = new URLSearchParams(forecastQuery(44.12345, -103.45678, "F"));
    expect(q.get("latitude")).toBe("44.123");
    expect(q.get("longitude")).toBe("-103.457");
    expect(q.get("forecast_days")).toBe("7");
    expect(q.get("temperature_unit")).toBe("fahrenheit");
    expect(new URLSearchParams(forecastQuery(1, 2, "C")).get("temperature_unit")).toBe("celsius");
    expect(q.get("wind_speed_unit")).toBe("mph");
  });
  it("WMO codes", () => {
    expect(WMO(0)).toEqual(["sun", "Clear"]);
    expect(WMO(2)[1]).toBe("Partly cloudy");
    expect(WMO(45)[1]).toBe("Fog");
    expect(WMO(55)[1]).toBe("Drizzle");
    expect(WMO(65)[1]).toBe("Rain");
    expect(WMO(75)[1]).toBe("Snow");
    expect(WMO(81)[1]).toBe("Showers");
    expect(WMO(85)[1]).toBe("Snow showers");
    expect(WMO(95)).toEqual(["wx_storm", "Thunderstorm"]);
  });
  it("rows: Today first, rounded temperatures, rain chance only from 10 %", () => {
    const r = forecastRows({ time: ["2026-11-23", "2026-11-24"], weather_code: [3, 61], temperature_2m_max: [40.6, 38.2],
      temperature_2m_min: [30.4, 28.5], precipitation_probability_max: [5, 40], wind_speed_10m_max: [4.4, 12.6] });
    expect(r[0]).toMatchObject({ day: "Today", label: "Overcast", hi: 41, lo: 30, pp: null, wind: 4 });
    expect(r[1]).toMatchObject({ label: "Rain", pp: 40, wind: 13 });
    expect(r[1]!.day).not.toBe("Today");
  });
});

describe("forecast loading", () => {
  afterEach(() => { _resetForecastModule(); vi.unstubAllGlobals(); });
  const daily = (tag: number) => ({ daily: { time: ["2026-11-23"], weather_code: [tag], temperature_2m_max: [1], temperature_2m_min: [0],
    precipitation_probability_max: [0], wind_speed_10m_max: [1] } });
  /** fetch that answers only when told to, recording the latitude asked for */
  function deferredFetch() {
    const calls: { lat: string; done: (code: number) => void }[] = [];
    vi.stubGlobal("fetch", (url: string) => new Promise(res => {
      const lat = new URL(url).searchParams.get("latitude")!;
      calls.push({ lat, done: code => res({ ok: true, json: async () => daily(code) }) });
    }));
    return calls;
  }
  const tick = () => new Promise(r => setTimeout(r, 0));

  it("a spot tapped while the van's forecast loads gets its own forecast as soon as that load ends", async () => {
    const calls = deferredFetch();
    void loadForecast(44.1, -103.5);                      // the van
    await tick();
    resetForecast();                                      // tap: forget it …
    void loadForecast(45.9, -101.2);                      // … and ask for the spot (a fetch is still running)
    expect(calls.map(c => c.lat)).toEqual(["44.100"]);
    calls[0]!.done(3);                                    // the van's answer arrives: discarded, the spot is fetched
    for (let i = 0; i < 5 && calls.length < 2; i++) await tick();
    expect(calls.map(c => c.lat)).toEqual(["44.100", "45.900"]);
    expect(forecastNow().fc).toBeNull();
    calls[1]!.done(61);
    for (let i = 0; i < 5 && !forecastNow().fc; i++) await tick();
    expect(forecastNow().fc!.weather_code).toEqual([61]);
  });

  it("due rules: within 30 min and 15 km it's reused; a > 15 km move refetches", async () => {
    const calls = deferredFetch();
    const p = loadForecast(44.1, -103.5);
    await tick(); calls[0]!.done(0); await p;
    await loadForecast(44.15, -103.5);                    // ~6 km away: reused
    expect(calls).toHaveLength(1);
    void loadForecast(45.1, -103.5);                      // ~110 km away: refetched
    await tick();
    expect(calls).toHaveLength(2);
  });
});
