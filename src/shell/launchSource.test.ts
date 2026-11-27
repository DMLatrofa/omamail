import { expect, test } from "nsh/test";
import { inboxSource, openingMode } from "./launchSource";

test("only the two exact Omamail inbox launch markers select an opening preference", () => {
  expect(inboxSource(["omamail://inbox/bar"])).toBe("bar");
  expect(inboxSource(["omamail://inbox/shortcut"])).toBe("shortcut");
  expect(inboxSource(["mailto:help@example.org", "omamail://inbox/bar"])).toBe(null);
  expect(inboxSource(["omamail://inbox/bar?mode=dropdown"])).toBe(null);
  expect(inboxSource(["--other", "omamail://inbox/bar"])).toBe(null);
});

test("only the default mail client can use the shortcut's drop-down preference", () => {
  const prefs = { openOnClick: "Drop-down", openOnShortcut: "Drop-down" } as const;
  expect(openingMode("bar", prefs, false)).toBe("Drop-down");
  expect(openingMode("shortcut", prefs, false)).toBe("Window");
  expect(openingMode("shortcut", prefs, true)).toBe("Drop-down");
  expect(openingMode(null, prefs, true)).toBe("Window");
});
