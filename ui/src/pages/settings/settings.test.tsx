// Settings logic: a Wi-Fi password typed on the on-screen keyboard goes only into its one request (never the store,
// a toast or the page), Cancel sends nothing, and the /api/settings loader. fetch is stubbed: nothing leaves the test.
import { act, cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { K, store } from "../../state/store";
import { loadSettings, resetSettingsData } from "./data";
import { Keyboard } from "./Keyboard";
import { wifiJoin } from "./SystemTabs";

let posts: { verb: string; args: Record<string, unknown> }[];
let gets: number;
beforeEach(() => {
  posts = []; gets = 0;
  store.set(K.control, true);
  vi.stubGlobal("fetch", vi.fn(async (url: string, init?: RequestInit) => {
    if (url === "/api/settings/action") posts.push(JSON.parse(String(init?.body)));
    else gets++;
    return { json: async () => ({ ok: true }) } as Response;
  }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); resetSettingsData(); });

const type = (root: HTMLElement, s: string) => {
  for (const ch of s) fireEvent.pointerDown(root.ownerDocument.querySelector(`#osk-keys [data-k="${ch}"]`)!);
};
const allStoreText = () => store.keys().map(k => JSON.stringify(store.get(k)) ?? "").join("\n");

describe("Wi-Fi password", () => {
  it("goes only into the wifi_connect request: not the store, not the page, not a toast", async () => {
    const { container } = render(<Keyboard />);
    let done!: Promise<void>;
    act(() => { done = wifiJoin({ ssid: "TestNet", security: "psk", known: false }); });
    const secret = "pw9x7q2kz";
    act(() => type(container, secret));
    const doc = container.ownerDocument;
    expect(doc.querySelector("#osk-text")!.textContent).toBe("•".repeat(secret.length));
    expect(doc.body.innerHTML).not.toContain(secret);
    expect(allStoreText()).not.toContain(secret);
    expect(posts).toHaveLength(0);
    await act(async () => { fireEvent.click(doc.querySelector('#osk-keys [data-a="done"]')!); await done; });
    expect(posts).toEqual([{ verb: "wifi_connect", args: { ssid: "TestNet", passphrase: secret } }]);
    expect(doc.querySelector("#osk")).toBeNull();
    expect(doc.body.innerHTML).not.toContain(secret);
    expect(allStoreText()).not.toContain(secret);                        // the toast says "Joining TestNet…"
    expect(store.get<{ msg: string }>(K.toast)?.msg).toBe("Joining TestNet: done");
  });

  it("Cancel sends nothing; too short a password can't be submitted", async () => {
    const { container } = render(<Keyboard />);
    let done!: Promise<void>;
    act(() => { done = wifiJoin({ ssid: "TestNet", security: "psk", known: false }); });
    act(() => type(container, "short"));
    const doc = container.ownerDocument;
    expect((doc.querySelector('#osk-keys [data-a="done"]') as HTMLButtonElement).disabled).toBe(true);
    await act(async () => { fireEvent.click(doc.querySelector("#osk-cancel")!); await done; });
    expect(posts).toHaveLength(0);
  });

  it("view only: the password is asked for, but nothing is posted", async () => {
    store.set(K.control, false);
    const { container } = render(<Keyboard />);
    let done!: Promise<void>;
    act(() => { done = wifiJoin({ ssid: "TestNet", security: "psk", known: false }); });
    act(() => type(container, "abcdefgh"));
    await act(async () => { fireEvent.click(container.ownerDocument.querySelector('#osk-keys [data-a="done"]')!); await done; });
    expect(posts).toHaveLength(0);
  });
});

describe("/api/settings loader", () => {
  it("fetches once per 10 s unless forced; a forced load during a fetch runs right after it", async () => {
    await loadSettings(false);
    await loadSettings(false);
    expect(gets).toBe(1);
    const a = loadSettings(true), b = loadSettings(true);                // b arrives while a is in flight
    await a; await b;
    await vi.waitFor(() => expect(gets).toBe(3));
  });
});
