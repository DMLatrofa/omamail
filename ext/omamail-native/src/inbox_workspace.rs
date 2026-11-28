//! The Omarchy inbox lives in a Hyprland special workspace while dropped down.
//! The platform owns animation, gaps, monitor selection and tiling.
use nsh_core::prelude::*;
#[cfg(target_os = "linux")]
use tokio::sync::oneshot;

const BAR: &str = "omamail://inbox/bar";
const SHORTCUT: &str = "omamail://inbox/shortcut";

fn launch_source(mut args: impl Iterator<Item = std::ffi::OsString>) -> Option<String> {
    let source = args.next()?;
    if args.next().is_some() {
        return None;
    }
    match source.to_str()? {
        BAR => Some("bar".into()),
        SHORTCUT => Some("shortcut".into()),
        _ => None,
    }
}

// Blocking compositor IPC and process discovery stay off the UI thread.
#[cfg(target_os = "linux")]
async fn background<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, &'static str> + Send + 'static,
) -> Result<T, String> {
    let (send, receive) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = send.send(task());
    });
    receive
        .await
        .map_err(|_| "workspace_unavailable".to_owned())?
        .map_err(str::to_owned)
}

/// Omarchy's optional inbox drop-down and its companion browser.
#[module]
pub struct InboxWorkspace;

#[module]
impl InboxWorkspace {
    /// The fixed bar or shortcut argument on the first launch, if present.
    pub fn launch_source() -> Option<String> {
        launch_source(std::env::args_os().skip(1))
    }

    /// Whether Hyprland can run the Lua dispatchers used by the drop-down.
    pub fn supported() -> bool {
        #[cfg(target_os = "linux")]
        return linux::supported();
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Updates the managed inbox binding when Omamail is still the default.
    pub async fn prepare_shortcut() -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        if Self::supported() {
            return background(crate::mail_client::prepare_shortcut).await;
        }
        Ok(false)
    }

    /// Toggles the drop-down or returns the inbox to the regular workspace.
    pub async fn present(drop_down: bool) -> Result<String, String> {
        #[cfg(target_os = "linux")]
        if Self::supported() {
            return background(move || linux::present(drop_down).map(str::to_owned)).await;
        }
        let _ = drop_down;
        Ok("window".to_owned())
    }

    /// Hides an active inbox workspace without closing its windows.
    pub async fn hide() -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        if Self::supported() {
            return background(linux::hide).await;
        }
        Ok(false)
    }

    /// Opens a confirmed public web URL next to the inbox. False delegates to the OS.
    pub async fn open_link(url: String) -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        if Self::supported() {
            return background(move || linux::open_link(&url)).await;
        }
        let _ = url;
        Ok(false)
    }

    /// Returns the companion browser to a regular workspace when Omamail quits.
    pub async fn release() -> Result<bool, String> {
        #[cfg(target_os = "linux")]
        if Self::supported() {
            return background(linux::release).await;
        }
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(test)]
mod tests {
    use super::launch_source;

    #[test]
    fn only_one_exact_inbox_launch_marker_is_accepted() {
        let parse = |args: &[&str]| launch_source(args.iter().map(|value| (*value).into()));
        assert_eq!(parse(&["omamail://inbox/bar"]).as_deref(), Some("bar"));
        assert_eq!(
            parse(&["omamail://inbox/shortcut"]).as_deref(),
            Some("shortcut")
        );
        assert_eq!(parse(&["omamail://inbox/bar?other=argument"]), None);
        assert_eq!(
            parse(&["mailto:a@example.org", "omamail://inbox/bar"]),
            None
        );
    }
}
