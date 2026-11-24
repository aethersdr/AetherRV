// /api/history for charts: each chart fetches only its own keys for its own span; the backend averages into
// buckets. Refreshed every 60 s while the chart is mounted (a hidden page's effects are paused).
import { useEffect, useState } from "react";
import type { Point } from "../charts/TimeChart";

export type History = Record<string, Point[]>;
const cache = new Map<string, History>();       // last data per request, so a page re-shown draws at once

export function useHistory(keys: readonly string[], since: number, points = 360, refreshMs = 60_000): History | undefined {
  const sig = `${keys.join(",")}|${since}|${points}`;
  const [data, setData] = useState<History | undefined>(() => cache.get(sig));
  useEffect(() => {
    let dead = false;
    setData(cache.get(sig));
    const load = async () => {
      try {
        const r = await fetch(`/api/history?keys=${keys.join(",")}&since=${since}&points=${points}`);
        const j = await r.json() as History;
        cache.set(sig, j);
        if (!dead) setData(j);
      } catch { /* backend restarting; keep the last data */ }
    };
    void load();
    const t = setInterval(load, refreshMs);
    return () => { dead = true; clearInterval(t); };
  }, [sig, refreshMs]);
  return data;
}
