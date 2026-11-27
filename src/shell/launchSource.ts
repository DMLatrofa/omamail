// The desktop launchers supply one fixed argument. It never carries an action
// or data picked by a message.
export type InboxSource = "bar" | "shortcut";
export type OpeningMode = "Window" | "Drop-down";

export function inboxSource(args: unknown): InboxSource | null {
  if (!Array.isArray(args) || args.length !== 1) return null;
  if (args[0] === "omamail://inbox/bar") return "bar";
  if (args[0] === "omamail://inbox/shortcut") return "shortcut";
  return null;
}

export function openingMode(
  source: InboxSource | null,
  preferences: { openOnClick: OpeningMode; openOnShortcut: OpeningMode },
  isDefault: boolean,
): OpeningMode {
  if (source === "bar") return preferences.openOnClick;
  return source === "shortcut" && isDefault ? preferences.openOnShortcut : "Window";
}
