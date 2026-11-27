import { InboxWorkspace } from "omamail-native";
import type { OpeningMode } from "./launchSource";

let inWorkspace = false;
let usedWorkspace = false;
let reconcileOnNextLaunch = false;
export type InboxPresentation = "window" | "shown" | "hidden";
let placement: Promise<InboxPresentation> = Promise.resolve("window");

export function dropDownSupported(): boolean {
  try {
    return InboxWorkspace.supported();
  } catch {
    return false;
  }
}

export function presentInbox(mode: OpeningMode): Promise<InboxPresentation> {
  placement = placement.then(() => placeInbox(mode));
  return placement;
}

async function placeInbox(mode: OpeningMode): Promise<InboxPresentation> {
  if (!dropDownSupported() || (mode === "Window" && !inWorkspace && !reconcileOnNextLaunch)) return "window";
  try {
    const result = await InboxWorkspace.present(mode === "Drop-down");
    reconcileOnNextLaunch = false;
    inWorkspace = result === "shown" || result === "hidden";
    usedWorkspace ||= inWorkspace;
    return result === "shown" || result === "hidden" ? result : "window";
  } catch {
    // A failed compositor call may have moved the window before reporting an error.
    reconcileOnNextLaunch = true;
    return "window";
  }
}

export async function dismissInbox(): Promise<boolean> {
  await placement;
  if (!inWorkspace) return false;
  try {
    return await InboxWorkspace.hide();
  } catch {
    return false;
  }
}

export async function openCompanionLink(url: string): Promise<boolean> {
  if (!inWorkspace) return false;
  // False means the system handler can open the URL. An error means a launch
  // was attempted, so let the reader show its failure without opening it twice.
  return InboxWorkspace.openLink(url);
}

export async function releaseCompanion(): Promise<void> {
  await placement;
  if (usedWorkspace) await InboxWorkspace.release();
}
