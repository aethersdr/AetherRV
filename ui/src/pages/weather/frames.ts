// The radar timeline's frames (app.js loadRadar): frames the Pi recorded (older than the source's 2 h; the weather
// service's /api/radar/recorded) + LibreWXR's 2 h of radar + its 1 h nowcast, or LibreWXR's hourly IR satellite.
// Pure functions; the controller (wxmap.ts) fetches and draws.

export type FrameKind = "rec" | "past" | "cast" | "ir";
export interface Frame { time: number; kind: FrameKind; minz: number; maxz: number; url: string }

/** LibreWXR's index (RainViewer-compatible). */
export interface WeatherMaps {
  host: string; generated?: number;
  radar: { past?: { time: number; path: string }[]; nowcast?: { time: number; path: string }[] };
  satellite?: { infrared?: { time: number; path: string }[] };
}
/** /api/radar/recorded */
export interface Recorded { frames?: number[]; flows?: number[]; zoom?: number[] }

export const LIBREWXR_INDEX = "https://api.librewxr.net/public/weather-maps.json";
export type WxLayer = "radar" | "ir" | "off";

/**
 * The frames to loop, oldest first. History reaches back `spanH` hours from "now" (the newest observed frame);
 * the forecast always follows. 512 px tiles, colour scheme 6 (NWS-style), no smoothing (AetherMap's settings);
 * forecast URLs carry the index's timestamp so a cached forecast tile is never shown once it's been observed.
 * minz/maxz: tile zooms that exist (recorded 4-7; source radar ≤ 10; IR satellite ≤ 6, hourly, colour 0).
 */
export function buildFrames(j: WeatherMaps, rec: Recorded, layer: "radar" | "ir", spanH: number): Frame[] {
  const past = j.radar?.past || [], cast = j.radar?.nowcast || [], oldest = past.length ? past[0]!.time : Infinity;
  const src = (path: string, q = "") => `${j.host}${path}/512/{z}/{x}/{y}/6/0_0.png${q}`;
  const frames: Frame[] = layer === "ir"
    ? (j.satellite?.infrared || []).map(f => ({ time: f.time, kind: "ir" as const, minz: 0, maxz: 6, url: `${j.host}${f.path}/512/{z}/{x}/{y}/0/0_0.png` }))
    : [
      ...(rec.frames || []).filter(t => t < oldest).map(t => ({ time: t, kind: "rec" as const, minz: 4, maxz: 7, url: `/api/radar/tile/${t}/{z}/{x}/{y}.png` })),
      ...past.map(f => ({ time: f.time, kind: "past" as const, minz: 0, maxz: 10, url: src(f.path) })),
      ...cast.map(f => ({ time: f.time, kind: "cast" as const, minz: 0, maxz: 10, url: src(f.path, `?g=${j.generated}`) })),
    ];
  const observed = frames.filter(f => f.kind !== "cast").map(f => f.time);
  const nowT = observed.length ? Math.max(...observed) : -Infinity;
  return frames.filter(f => f.kind === "cast" || f.time >= nowT - spanH * 3600);
}

/** Index of "now": the newest observed radar frame (IR: the last frame). */
export function nowIndex(frames: readonly Frame[]): number {
  const i = frames.map(f => f.kind).lastIndexOf("past");
  return i < 0 ? frames.length - 1 : i;
}

export const fmtAgo = (m: number) => m < 60 ? `${m} min ago` : `${Math.floor(m / 60)} h${m % 60 ? ` ${m % 60} min` : ""} ago`;

/** The time label under the timeline for frame f. */
export function frameLabel(f: Frame, now = Date.now()): string {
  const t = new Date(f.time * 1000), mins = Math.round((t.getTime() - now) / 60e3);
  const hm = t.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return f.kind === "cast" ? `${hm} · forecast +${Math.max(0, mins)} min` : `${f.kind === "ir" ? "IR · " : ""}${hm} · ${fmtAgo(Math.max(0, -mins))}`;
}

/** Where to put the timeline after a refresh: "now" stays "now"; otherwise the same time (by frame time). */
export function keepPosition(frames: readonly Frame[], prevT: number | null, wasNow: boolean, force: boolean): number {
  const now = nowIndex(frames);
  if (wasNow || force || prevT == null) return now;
  const k = frames.findIndex(f => f.time >= prevT);
  return k < 0 ? frames.length - 1 : k;
}

/** The recorded flow nearest frame pair (a, b) and its scale to this pair's gap, or null (app.js flowFor). */
export function nearestFlow(flowTimes: readonly number[], aTime: number): number | null {
  if (!flowTimes.length) return null;
  const ft = flowTimes.reduce((best, t) => Math.abs(t - aTime) < Math.abs(best - aTime) ? t : best, flowTimes[0]!);
  return Math.abs(ft - aTime) > 3 * 3600 ? null : ft;
}

export const RV_SPANS = [1, 2, 6] as const;
export const RV_SPEEDS = [1, 2, 4] as const;
/** 1× = 700 ms a frame. */
export const frameMs = (speed: number) => 700 / speed;
