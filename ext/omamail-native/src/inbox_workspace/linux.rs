use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Mutex,
    thread::sleep,
    time::{Duration, Instant},
};

pub(super) const WORKSPACE: &str = "special:omamail-inbox";
const TOGGLE: &str = r#"hl.dsp.workspace.toggle_special("omamail-inbox")"#;
static VISIBILITY: Mutex<()> = Mutex::new(());
static BROWSER_LAUNCH: Mutex<()> = Mutex::new(());
static BROWSER: Mutex<Option<Companion>> = Mutex::new(None);

fn output(program: &str, args: &[&str]) -> Result<String, &'static str> {
    let reply = Command::new("timeout")
        .args(["2s", program])
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "workspace_unavailable")?;
    if !reply.status.success() {
        return Err("workspace_unavailable");
    }
    String::from_utf8(reply.stdout).map_err(|_| "workspace_unavailable")
}

fn query(key: &str) -> Result<Value, &'static str> {
    serde_json::from_str(&output("hyprctl", &["-j", key])?).map_err(|_| "workspace_unavailable")
}

fn dispatch(actions: &[String]) -> Result<(), &'static str> {
    let code = actions
        .iter()
        .map(|action| format!("hl.dispatch({action})"))
        .collect::<Vec<_>>()
        .join("; ");
    if output("hyprctl", &["eval", &code])?.trim() == "ok" {
        Ok(())
    } else {
        Err("workspace_unavailable")
    }
}

fn array(value: &Value) -> Result<&[Value], &'static str> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or("workspace_unavailable")
}

fn address(client: &Value) -> Option<&str> {
    let address = client["address"].as_str()?;
    let hex = address.strip_prefix("0x")?;
    (!hex.is_empty() && hex.len() <= 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(address)
}

fn belongs(client: &Value) -> bool {
    client["workspace"]["name"] == WORKSPACE
}

fn is_mail(client: &Value) -> bool {
    client["pid"].as_u64() == Some(std::process::id().into())
        && client["initialTitle"] == "Omamail"
        && address(client).is_some()
}

fn wait_client(
    mut matches: impl FnMut(&Value) -> bool,
    seconds: u64,
) -> Result<Value, &'static str> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let clients = query("clients")?;
        if let Some(client) = array(&clients)?.iter().find(|client| matches(client)) {
            return Ok(client.clone());
        }
        if Instant::now() >= deadline {
            return Err("workspace_window_missing");
        }
        sleep(Duration::from_millis(100));
    }
}

fn visible_monitor(monitors: &[Value]) -> Option<&Value> {
    monitors
        .iter()
        .find(|monitor| monitor["specialWorkspace"]["name"] == WORKSPACE)
}

fn regular_workspace(monitors: &[Value]) -> Option<i64> {
    monitors.iter().find(|monitor| monitor["focused"] == true)?["activeWorkspace"]["id"]
        .as_i64()
        .filter(|id| *id > 0)
}

fn move_window(client: &Value, destination: &str) -> Result<String, &'static str> {
    let address = address(client).ok_or("workspace_window_missing")?;
    Ok(format!(
        r#"hl.dsp.window.move({{ workspace = "{destination}", follow = false, window = "address:{address}" }})"#
    ))
}

fn place(client: &Value) -> Result<(), &'static str> {
    let mut actions = Vec::new();
    if !belongs(client) {
        actions.push(move_window(client, WORKSPACE)?);
    }
    if client["floating"] == true {
        let address = address(client).ok_or("workspace_window_missing")?;
        actions.push(format!(
            r#"hl.dsp.window.float({{ action = "off", window = "address:{address}" }})"#
        ));
    }
    if actions.is_empty() {
        Ok(())
    } else {
        dispatch(&actions)
    }
}

fn toggle(monitor: Option<&Value>) -> Result<(), &'static str> {
    let mut actions = Vec::new();
    if let Some(monitor) = monitor.filter(|monitor| monitor["focused"] != true) {
        let name = monitor["name"]
            .as_str()
            .filter(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            })
            .ok_or("workspace_unavailable")?;
        actions.push(format!(r#"hl.dsp.focus({{ monitor = "{name}" }})"#));
    }
    actions.push(TOGGLE.into());
    dispatch(&actions)
}

pub(super) fn present(drop_down: bool) -> Result<&'static str, &'static str> {
    let _serial = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
    let client = wait_client(is_mail, 3)?;
    let monitors = query("monitors")?;
    let monitors = array(&monitors)?;
    let visible = visible_monitor(monitors);
    if drop_down {
        let was_placed = belongs(&client);
        place(&client)?;
        if (visible.is_none() || was_placed)
            && let Err(error) = toggle(visible)
        {
            if !was_placed
                && let Some(id) = client["workspace"]["id"].as_i64().filter(|id| *id > 0)
                && let Ok(action) = move_window(&client, &id.to_string())
            {
                let _ = dispatch(&[action]);
            }
            return Err(error);
        }
        return Ok(if was_placed && visible.is_some() {
            "hidden"
        } else {
            "shown"
        });
    }
    if belongs(&client) {
        let workspace = regular_workspace(monitors).ok_or("workspace_unavailable")?;
        dispatch(&[move_window(&client, &workspace.to_string())?])?;
        if let Some(monitor) = visible {
            toggle(Some(monitor))?;
        }
        let _ = release();
    }
    Ok("window")
}

pub(super) fn hide() -> Result<bool, &'static str> {
    let _serial = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
    let clients = query("clients")?;
    if !array(&clients)?
        .iter()
        .any(|client| is_mail(client) && belongs(client))
    {
        return Ok(false);
    }
    let monitors = query("monitors")?;
    if let Some(monitor) = visible_monitor(array(&monitors)?) {
        toggle(Some(monitor))?;
    }
    Ok(true)
}

struct Browser {
    desktop: &'static str,
    commands: &'static [&'static str],
    data_dir: &'static str,
}

const BROWSERS: [Browser; 3] = [
    Browser {
        desktop: "brave-browser.desktop",
        commands: &["brave", "brave-browser"],
        data_dir: "BraveSoftware/Brave-Browser",
    },
    Browser {
        desktop: "chromium.desktop",
        commands: &["chromium"],
        data_dir: "chromium",
    },
    Browser {
        desktop: "google-chrome.desktop",
        commands: &["google-chrome-stable", "google-chrome"],
        data_dir: "google-chrome",
    },
];

#[derive(Clone)]
struct Companion {
    desktop: &'static str,
    title: String,
    profile: Option<String>,
}

impl Companion {
    fn matches(&self, client: &Value) -> bool {
        client["title"].as_str().is_some_and(|title| {
            title == self.title
                || title
                    .strip_prefix(&self.title)
                    .is_some_and(|suffix| suffix.starts_with(" - "))
        }) && address(client).is_some()
    }
}

fn browser_profile(browser: &Browser) -> Option<String> {
    let absolute = |name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let directory = absolute("CHROME_USER_DATA_DIR").or_else(|| {
        absolute("CHROME_CONFIG_HOME")
            .or_else(|| absolute("XDG_CONFIG_HOME"))
            .or_else(|| absolute("HOME").map(|home| home.join(".config")))
            .map(|config| config.join(browser.data_dir))
    })?;
    let mut data = Vec::new();
    File::open(directory.join("Local State"))
        .ok()?
        .take(8 * 1024 * 1024)
        .read_to_end(&mut data)
        .ok()?;
    let local: Value = serde_json::from_slice(&data).ok()?;
    let name = local["profile"]["last_used"].as_str()?;
    let numbered = name
        .strip_prefix("Profile ")
        .is_some_and(|n| !n.is_empty() && n.len() <= 7 && n.bytes().all(|b| b.is_ascii_digit()));
    ((name == "Default" || numbered) && directory.join(name).is_dir()).then(|| name.into())
}

fn browser_title() -> Result<String, &'static str> {
    let mut bytes = [0u8; 12];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| "workspace_browser_unavailable")?;
    Ok(format!(
        "Omamail {}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn browser_spawn(
    browser: &Browser,
    profile: Option<&str>,
    title: Option<&str>,
    url: &str,
) -> Result<(), &'static str> {
    for executable in browser.commands {
        let mut command = Command::new(executable);
        if let Some(profile) = profile {
            command.arg(format!("--profile-directory={profile}"));
        }
        if let Some(title) = title {
            command
                .arg("--new-window")
                .arg(format!("--window-name={title}"));
        }
        // The confirmed, checked URL crosses only the selected browser's argv.
        if let Ok(mut child) = command
            .args(["--", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
    }
    Err("workspace_browser_unavailable")
}

fn valid_link(url: &str) -> bool {
    url.len() <= 32 * 1024
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
        && url.split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
        })
        && omamail::message::html::is_public_url(url)
}

fn focus_browser(window: &Value) -> Result<(), &'static str> {
    let address = address(window).ok_or("workspace_window_missing")?;
    dispatch(&[format!(
        r#"hl.dsp.focus({{ window = "address:{address}" }})"#
    )])?;
    let deadline = Instant::now() + Duration::from_secs(1);
    while query("activewindow")?["address"] != address {
        if Instant::now() >= deadline {
            return Err("workspace_browser_unavailable");
        }
        sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn mail_in_workspace(clients: &[Value]) -> bool {
    clients
        .iter()
        .any(|client| is_mail(client) && belongs(client))
}

fn release_window(window: &Value) -> Result<(), &'static str> {
    let monitors = query("monitors")?;
    let workspace = regular_workspace(array(&monitors)?).ok_or("workspace_unavailable")?;
    dispatch(&[move_window(window, &workspace.to_string())?])
}

pub(super) fn open_link(url: &str) -> Result<bool, &'static str> {
    if !valid_link(url) {
        return Err("workspace_invalid_link");
    }
    let Ok(_launch) = BROWSER_LAUNCH.lock() else {
        return Ok(false);
    };
    let previous_companion = BROWSER.lock().ok().and_then(|state| state.clone());
    let Ok(payload) = query("clients") else {
        return Ok(false);
    };
    let Ok(clients) = array(&payload) else {
        return Ok(false);
    };
    if !mail_in_workspace(clients) {
        return Ok(false);
    }
    let Ok(desktop) = output("omarchy-cmd-default-browser", &[])
        .or_else(|_| output("xdg-settings", &["get", "default-web-browser"]))
    else {
        return Ok(false);
    };
    let Some(browser) = BROWSERS
        .iter()
        .find(|browser| browser.desktop == desktop.trim())
    else {
        return Ok(false);
    };
    if let Some(previous) = previous_companion.as_ref()
        && let Some(window) = clients
            .iter()
            .find(|client| belongs(client) && previous.matches(client))
    {
        if previous.desktop == browser.desktop {
            return Ok(focus_browser(window)
                .and_then(|()| browser_spawn(browser, previous.profile.as_deref(), None, url))
                .is_ok());
        }
        if release_window(window).is_err() {
            return Ok(false);
        }
        if let Ok(mut own) = BROWSER.lock() {
            *own = None;
        }
    }
    let Ok(title) = browser_title() else {
        return Ok(false);
    };
    let companion = Companion {
        desktop: browser.desktop,
        title,
        profile: browser_profile(browser),
    };
    let previous = clients.iter().filter_map(address).collect::<Vec<_>>();
    if browser_spawn(
        browser,
        companion.profile.as_deref(),
        Some(&companion.title),
        url,
    )
    .is_err()
    {
        return Ok(false);
    }
    let window = wait_client(
        |client| {
            companion.matches(client) && !address(client).is_some_and(|a| previous.contains(&a))
        },
        7,
    )?;
    let _serial = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
    if mail_in_workspace(array(&query("clients")?)?) {
        place(&window)?;
        *BROWSER.lock().map_err(|_| "workspace_unavailable")? = Some(companion);
    } else if belongs(&window) {
        release_window(&window)?;
    }
    Ok(true)
}

pub(super) fn release() -> Result<bool, &'static str> {
    let mut own = BROWSER.lock().map_err(|_| "workspace_unavailable")?;
    let Some(companion) = own.as_ref() else {
        return Ok(false);
    };
    let clients = query("clients")?;
    if let Some(window) = array(&clients)?
        .iter()
        .find(|client| belongs(client) && companion.matches(client))
    {
        release_window(window)?;
    }
    *own = None;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_links_reach_only_the_browser_http_boundary() {
        for url in [
            "file:///etc/passwd",
            "--guest",
            "https://127.0.0.1/",
            "https://example.org/\n",
            "https://",
        ] {
            assert!(!valid_link(url), "{url:?}");
        }
        assert!(valid_link("https://example.org/test?a='a'"));
    }

    #[test]
    fn compositor_selectors_only_use_valid_hex_addresses() {
        assert_eq!(
            address(&serde_json::json!({"address": "0xabcdef"})),
            Some("0xabcdef")
        );
        assert_eq!(
            address(&serde_json::json!({"address": "0x0\" escape"})),
            None
        );
    }
}
