import { mount, signal, Text, View } from "nsh";
import { Window } from "nsh/desktop";
import { settings } from "../../features/settings";
import { Button } from "../../ui";
import { dismissInbox, openInboxLink, presentInbox } from "../dropDown";
import { Shell } from "../Shell";

mount(() => {
  const fallbackResult = signal("");
  async function openFallback(dismiss: boolean) {
    const pending = openInboxLink("https://example.org/", async () => "System browser opened");
    if (dismiss) await dismissInbox();
    fallbackResult.set(await pending ?? "System browser skipped");
  }
  settings.preview({ openOnClick: "Drop-down" });
  void presentInbox("Drop-down");
  return (
    <Window id="main" title="Omamail">
      <View class="flex-col w-full h-full">
        <Button testID="drop-down-reopen" label="Hide and reopen" onPress={() => {
          void dismissInbox();
          void presentInbox("Drop-down");
        }} />
        <Button testID="drop-down-window-retry" label="Switch, minimize and retry" onPress={async () => {
          await presentInbox("Window");
          await dismissInbox();
          await presentInbox("Window");
        }} />
        <Button testID="drop-down-cancel-fallback" label="Open link and hide" onPress={() => void openFallback(true)} />
        <Button testID="drop-down-system-browser" label="Open link" onPress={() => void openFallback(false)} />
        <Text>{fallbackResult()}</Text>
        <Shell launchLinks={false} />
      </View>
    </Window>
  );
});
