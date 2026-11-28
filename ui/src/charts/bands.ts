// Run bands for a time chart: the stretches when something was running, from a history series whose buckets
// average to > 0 while it runs (e.g. the MCS7's fuel-pump rate, the Espar's state code). /api/history averages
// into buckets of max(30 s, since / points) and stamps each with its last sample, so a bucket with any running in it
// covers [t - bucket, t]; neighbouring buckets merge into one band.
import type { Point } from "./TimeChart";

export type Span = readonly [number, number];

/** The bucket width /api/history uses for `since` seconds in `points` points (the gateway's `history_get`). */
export const bucketS = (since: number, points: number) => Math.max(30, since / Math.max(1, points));

export function runSpans(pts: readonly Point[] | undefined, bucket: number): Span[] {
  const out: [number, number][] = [];
  for (const [t, v] of pts ?? []) {
    if (!(v > 0)) continue;
    const last = out[out.length - 1];
    if (last && t - bucket <= last[1] + bucket / 2) last[1] = t;
    else out.push([t - bucket, t]);
  }
  return out;
}

/** Whether `t` falls in one of `spans`. */
export const inSpan = (spans: readonly Span[], t: number) => spans.some(([a, z]) => t >= a && t <= z);
