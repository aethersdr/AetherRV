// Shell feedback shared by every page: the toast and the confirmation dialog (app.js toast / askConfirm).
import { K, store } from "../state/store";

export type ToastKind = "" | "ok" | "bad" | "busy";
export interface ToastState { msg: string; kind: ToastKind; seq: number }
let toastSeq = 0;

/** One line at the bottom of the screen: 4 s ("busy": 20 s, or until the next toast). */
export function toast(msg: string, kind: ToastKind = ""): void {
  store.set(K.toast, { msg, kind, seq: ++toastSeq } satisfies ToastState);
}

export interface ConfirmState { title: string; detail: string; onYes: () => void }

/** Ask before a heat / fan / critical action. onYes runs from the Confirm button's click (a user gesture). */
export function askConfirm(title: string, detail: string, onYes: () => void): void {
  store.set(K.confirm, { title, detail, onYes } satisfies ConfirmState);
}
export function closeConfirm(): void { store.set(K.confirm, null); }

export type ModalName = "batt" | "inv" | null;
export function openModal(m: ModalName): void { store.set(K.modal, m); }
