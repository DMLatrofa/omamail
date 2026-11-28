import { InboxWorkspace } from "omamail-native";
import type { OpeningMode } from "./launchSource";

let inWorkspace = false;
let usedWorkspace = false;
let quitting = false;
let reconcileOnNextLaunch = false;
let dismissals = 0;
export type InboxPresentation = "window" | "shown" | "hidden";
let placement = Promise.resolve();

function enqueue<T>(run: () => Promise<T>): Promise<T> {
  const result = placement.then(run);
  placement = result.then(() => {}, () => {});
  return result;
}

export function dropDownSupported(): boolean {
  try {
    return InboxWorkspace.supported();
  } catch {
    return false;
  }
}

export function presentInbox(mode: OpeningMode): Promise<InboxPresentation> {
  if (quitting) return Promise.resolve("window");
  return enqueue(() => placeInbox(mode));
}

async function placeInbox(mode: OpeningMode): Promise<InboxPresentation> {
  if (!dropDownSupported() || (mode === "Window" && !inWorkspace && !reconcileOnNextLaunch)) return "window";
  try {
    const result = await InboxWorkspace.present(mode === "Drop-down");
    reconcileOnNextLaunch = false;
    inWorkspace = result === "shown" || result === "hidden";
    usedWorkspace ||= inWorkspace;
    if (result === "hidden") dismissals += 1;
    return result === "shown" || result === "hidden" ? result : "window";
  } catch {
    // A failed compositor call may have moved the window before reporting an error.
    reconcileOnNextLaunch = true;
    return "window";
  }
}

type InboxDismissal = "window" | "hidden" | "failed";

export function dismissInbox(): Promise<InboxDismissal> {
  if (quitting) return Promise.resolve("hidden");
  return enqueue(hideInbox);
}

async function hideInbox(): Promise<InboxDismissal> {
  if (!inWorkspace && !reconcileOnNextLaunch) return "window";
  try {
    inWorkspace = await InboxWorkspace.hide();
    // A dismissal does not finish a Window transition that already moved the inbox.
    usedWorkspace ||= inWorkspace;
    if (inWorkspace) {
      dismissals += 1;
      return "hidden";
    }
    return "window";
  } catch {
    reconcileOnNextLaunch = true;
    return "failed";
  }
}

export async function openInboxLink<T>(
  url: string,
  openWithSystem: (url: string) => Promise<T>,
): Promise<T | undefined> {
  if (quitting) throw new Error("Omamail is quitting");
  if (!inWorkspace) return openWithSystem(url);
  const dismissal = dismissals;
  if (await InboxWorkspace.openLink(url)) return;
  // A dismissal may be queued while the native result or an earlier
  // placement is still pending. Give those commands a chance to finish.
  let queued: Promise<void>;
  do {
    queued = placement;
    await queued;
  } while (queued !== placement);
  if (quitting || dismissals !== dismissal) return;
  return openWithSystem(url);
}

export async function releaseCompanion(): Promise<void> {
  quitting = true;
  await placement;
  if (usedWorkspace) await InboxWorkspace.release();
}
