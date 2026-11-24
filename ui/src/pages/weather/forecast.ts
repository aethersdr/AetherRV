// 7-day forecast from Open-Meteo (free, no key), as app.js loadForecast / renderForecast: refetched every 30 min or
// after a > 15 km move; after a failure it waits 2 min before trying again. Module-level, so the last forecast
// survives the page being hidden (and remounted).
import { useEffect, useState } from "react";
import { UNIT } from "../../lib/format";
import { kmBetween, type LatLon } from "./geo";

export interface Daily {
  time: string[]; weather_code: number[]; temperature_2m_max: number[]; temperature_2m_min: number[];
  precipitation_probability_max: (number | null)[]; wind_speed_10m_max: number[];
}
export interface FcState { fc: Daily | null; err: string | null }

const S = { fc: null as Daily | null, at: 0, atPos: null as LatLon | null, busy: false, err: null as string | null,
  gen: 0, want: null as LatLon | null };
const listeners = new Set<() => void>();
const notify = () => listeners.forEach(l => l());

export const FORECAST_URL = "https://api.open-meteo.com/v1/forecast";

export function forecastQuery(lat: number, lon: number, unit: "F" | "C" = UNIT): string {
  return new URLSearchParams({ latitude: lat.toFixed(3), longitude: lon.toFixed(3), forecast_days: "7", timezone: "auto",
    daily: "weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,wind_speed_10m_max",
    temperature_unit: unit === "F" ? "fahrenheit" : "celsius", wind_speed_unit: "mph" }).toString();
}

/** A new spot was picked (or the pick dropped): forget the forecast so the next load fetches at once. */
export function resetForecast(): void {
  S.fc = null; S.at = 0; S.err = null; S.gen++;          // a fetch still running for the old spot is discarded
  notify();
}

/**
 * Fetch if due (same rules as app.js). Never throws. One fetch at a time: a call while one runs only records the
 * spot wanted, and the running fetch checks it when it ends (so a tap during a load gets that spot's forecast at
 * once, not at the next check); a result for a spot dropped by resetForecast() is discarded.
 */
export async function loadForecast(lat: number, lon: number, now = Date.now()): Promise<void> {
  S.want = [lat, lon];
  if (S.busy || (S.fc && now - S.at < 30 * 60e3 && S.atPos && kmBetween(S.atPos, [lat, lon]) <= 15)) return;
  if (S.err && now - S.at < 2 * 60e3) return;
  S.busy = true;
  const gen = S.gen;
  let daily: Daily | null = null, err: string | null = null;
  try {
    const r = await fetch(`${FORECAST_URL}?${forecastQuery(lat, lon)}`);
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    daily = (await r.json() as { daily?: Daily }).daily ?? null;
    if (!daily || !Array.isArray(daily.time)) throw new Error("no daily forecast");
  } catch (e) { daily = null; err = String((e as Error)?.message || e); }
  S.busy = false;
  if (gen === S.gen) {
    if (daily) { S.fc = daily; S.err = null; S.atPos = [lat, lon]; } else S.err = err;
    S.at = Date.now();
    notify();
  }
  const w = S.want;
  if (w && (gen !== S.gen || w[0] !== lat || w[1] !== lon)) await loadForecast(w[0], w[1]);
}

/** The current forecast, outside React (tests). */
export const forecastNow = (): FcState => ({ fc: S.fc, err: S.err });

/** The current forecast (re-renders when it changes). */
export function useForecast(): FcState {
  const [v, setV] = useState<FcState>(() => ({ fc: S.fc, err: S.err }));
  useEffect(() => {
    const f = () => setV(p => (p.fc === S.fc && p.err === S.err ? p : { fc: S.fc, err: S.err }));
    listeners.add(f);
    f();
    return () => { listeners.delete(f); };
  }, []);
  return v;
}

/** WMO weather code → [icon, label] (app.js WMO). */
export const WMO = (c: number): [string, string] => c === 0 ? ["sun", "Clear"] : c <= 2 ? ["wx_partly", "Partly cloudy"] : c === 3 ? ["wx_cloud", "Overcast"]
  : c <= 48 ? ["wx_fog", "Fog"] : c <= 57 ? ["wx_rain", "Drizzle"] : c <= 67 ? ["wx_rain", "Rain"] : c <= 77 ? ["wx_snow", "Snow"]
  : c <= 82 ? ["wx_rain", "Showers"] : c <= 86 ? ["wx_snow", "Snow showers"] : ["wx_storm", "Thunderstorm"];

export interface FcRow { key: string; day: string; label: string; icon: string; hi: number; lo: number; pp: number | null; wind: number }

/** The rows the forecast column shows. */
export function forecastRows(fc: Daily): FcRow[] {
  return fc.time.map((d, i) => {
    const dt = new Date(`${d}T12:00`), [icon, label] = WMO(fc.weather_code[i] ?? 0);
    const pp = fc.precipitation_probability_max[i];
    return { key: d, day: i === 0 ? "Today" : dt.toLocaleDateString([], { weekday: "short" }), label, icon,
      hi: Math.round(fc.temperature_2m_max[i] ?? NaN), lo: Math.round(fc.temperature_2m_min[i] ?? NaN),
      pp: pp != null && pp >= 10 ? pp : null, wind: Math.round(fc.wind_speed_10m_max[i] ?? NaN) };
  });
}

/** Tests only. */
export function _resetForecastModule(): void {
  S.fc = null; S.at = 0; S.atPos = null; S.busy = false; S.err = null; S.gen = 0; S.want = null;
}
