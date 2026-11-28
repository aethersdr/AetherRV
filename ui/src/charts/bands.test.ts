import { describe, expect, it } from "vitest";
import { bucketS, inSpan, runSpans } from "./bands";

describe("heater run bands", () => {
  it("uses the gateway's bucket width", () => {
    expect(bucketS(86400, 360)).toBe(240);
    expect(bucketS(3600, 360)).toBe(30);          // never under 30 s
    expect(bucketS(2592000, 360)).toBe(7200);
  });

  it("merges neighbouring running buckets into one band and skips the idle ones", () => {
    const b = 240;
    const pts = [[1000, 0], [1240, 5.5], [1480, 5.2], [1720, 0], [1960, 0], [2200, 0.04], [2440, 0]] as const;
    expect(runSpans(pts, b)).toEqual([[1000, 1480], [1960, 2200]]);
  });

  it("a state code that averages above 0 counts (the Espar: off 0, heating 2, …); nothing gives no bands", () => {
    expect(runSpans([[500, 0], [740, 1.5], [980, 2], [1220, 2]], 240)).toEqual([[500, 1220]]);
    expect(runSpans(undefined, 240)).toEqual([]);
    expect(runSpans([[500, 0], [740, 0]], 240)).toEqual([]);
  });

  it("a gap in the samples splits a band", () => {
    expect(runSpans([[1000, 3], [1240, 3], [5000, 3]], 240)).toEqual([[760, 1240], [4760, 5000]]);
    expect(inSpan([[760, 1240]], 1000)).toBe(true);
    expect(inSpan([[760, 1240]], 1300)).toBe(false);
  });
});
