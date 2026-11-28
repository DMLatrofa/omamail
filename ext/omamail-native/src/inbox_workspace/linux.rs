use serde_json::Value;
use socket2::{Domain, SockAddr, Socket, Type};
use std::{
    fs::File,
    io::{Read, Write},
    net::Shutdown,
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::MetadataExt, net::UnixStream},
    },
    path::Path,
    process::{Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::sleep,
    time::{Duration, Instant},
};

pub(super) const WORKSPACE: &str = "special:omamail-inbox";
const TOGGLE: &str = r#"hl.dsp.workspace.toggle_special("omamail-inbox")"#;
struct Visibility {
    // Keep the destination of an unfinished transition to a regular window.
    destination: Option<String>,
    dismissed: bool,
    dismissal_generation: u64,
}

impl Visibility {
    fn dismiss(&mut self) {
        self.dismissed = true;
        self.dismissal_generation = self.dismissal_generation.wrapping_add(1);
    }
}

static VISIBILITY: Mutex<Visibility> = Mutex::new(Visibility {
    destination: None,
    dismissed: false,
    dismissal_generation: 0,
});
static BROWSER_LAUNCH: Mutex<()> = Mutex::new(());
static BROWSER: Mutex<Option<Companion>> = Mutex::new(None);
static QUITTING: AtomicBool = AtomicBool::new(false);

pub(super) fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some_and(|signature| !signature.is_empty())
        && *SUPPORTED.get_or_init(|| {
            matches!(
                output_with_timeout("0.5s", "hyprctl", &["eval",
                    "assert(type(hl.dispatch) == 'function' and type(hl.dsp.workspace.toggle_special) == 'function')"]),
                Ok(reply) if reply.trim() == "ok"
            )
        })
}

fn output(program: &str, args: &[&str]) -> Result<String, &'static str> {
    output_with_timeout("2s", program, args)
}

fn output_with_timeout(limit: &str, program: &str, args: &[&str]) -> Result<String, &'static str> {
    let reply = Command::new("timeout")
        .args([limit, program])
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

fn workspace_selector(workspace: &Value) -> Option<String> {
    match workspace["id"].as_i64()? {
        id if id > 0 => Some(id.to_string()),
        id if id < 0 => {
            let name = workspace["name"].as_str()?;
            (!name.is_empty()
                && name != "special"
                && !name.starts_with("special:")
                && !name.chars().any(char::is_control))
            .then(|| format!("name:{name}"))
        }
        _ => None,
    }
}

fn regular_workspace(monitors: &[Value]) -> Option<String> {
    let monitor = monitors.iter().find(|monitor| monitor["focused"] == true)?;
    workspace_selector(&monitor["activeWorkspace"])
}

fn move_window(client: &Value, destination: &str) -> Result<String, &'static str> {
    let address = address(client).ok_or("workspace_window_missing")?;
    let destination = destination.replace('\\', r"\\").replace('"', r#"\""#);
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
    let mut visibility = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
    let mut client = wait_client(is_mail, 3)?;
    if !drop_down || visibility.destination.is_some() {
        switch_to_window(&client, &mut visibility.destination)?;
        if !drop_down {
            return Ok("window");
        }
        client = wait_client(is_mail, 3)?;
    }
    let monitors = query("monitors")?;
    let monitors = array(&monitors)?;
    let visible = visible_monitor(monitors);
    let was_placed = belongs(&client);
    place(&client)?;
    if (visible.is_none() || was_placed)
        && let Err(error) = toggle(visible)
    {
        if !was_placed
            && let Some(workspace) = workspace_selector(&client["workspace"])
            && let Ok(action) = move_window(&client, &workspace)
        {
            let _ = dispatch(&[action]);
        }
        return Err(error);
    }
    if was_placed && visible.is_some() {
        visibility.dismiss();
        Ok("hidden")
    } else {
        visibility.dismissed = false;
        Ok("shown")
    }
}

fn switch_to_window(
    client: &Value,
    pending_destination: &mut Option<String>,
) -> Result<(), &'static str> {
    let in_workspace = belongs(client);
    if pending_destination.is_none() && in_workspace {
        let monitors = query("monitors")?;
        *pending_destination =
            Some(regular_workspace(array(&monitors)?).ok_or("workspace_unavailable")?);
    }
    let Some(destination) = pending_destination.as_deref() else {
        return Ok(());
    };
    if in_workspace {
        dispatch(&[move_window(client, destination)?])?;
    }
    let monitors = query("monitors")?;
    if let Some(monitor) = visible_monitor(array(&monitors)?) {
        toggle(Some(monitor))?;
    }
    release_companion(Some(destination))?;
    *pending_destination = None;
    Ok(())
}

pub(super) fn hide() -> Result<bool, &'static str> {
    let mut visibility = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
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
    visibility.dismiss();
    Ok(true)
}

struct Browser {
    desktop: &'static str,
    commands: &'static [&'static str],
}

const BROWSERS: [Browser; 3] = [
    Browser {
        desktop: "brave-browser.desktop",
        commands: &["brave", "brave-browser"],
    },
    Browser {
        desktop: "chromium.desktop",
        commands: &["chromium"],
    },
    Browser {
        desktop: "google-chrome.desktop",
        commands: &["google-chrome-stable", "google-chrome"],
    },
];

#[derive(Clone)]
struct Companion {
    desktop: &'static str,
    title: String,
}

impl Companion {
    fn matches(&self, client: &Value) -> bool {
        address(client).is_some()
            && ["initialTitle", "title"].iter().any(|key| {
                client[*key].as_str().is_some_and(|title| {
                    title == self.title
                        || title
                            .strip_prefix(&self.title)
                            .is_some_and(|suffix| suffix.starts_with(" - "))
                })
            })
    }
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

fn browser_spawn(browser: &Browser, title: &str) -> Result<(), &'static str> {
    // A titled bootstrap page lets Hyprland identify the new window even
    // before Chromium displays its custom window name.
    let bootstrap = format!(
        "data:text/html,%3Ctitle%3E{}%3C%2Ftitle%3E",
        title.replace(' ', "%20")
    );
    for executable in browser.commands {
        let mut command = Command::new(executable);
        // The long-lived browser process can keep this command line.
        if let Ok(mut child) = command
            .arg("--new-window")
            .arg(format!("--window-name={title}"))
            .args(["--", &bootstrap])
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

fn singleton_paths(table: &[u8]) -> impl Iterator<Item = &Path> {
    table.split(|byte| *byte == b'\n').filter_map(|line| {
        let mut fields = line.splitn(7, |byte| *byte == b' ');
        // Only listening stream sockets. The final field includes the path
        // and can contain spaces, so stop splitting before the inode.
        if fields.nth(3)? != b"00010000" || fields.next()? != b"0001" || fields.next()? != b"01" {
            return None;
        }
        let tail = fields.next()?.trim_ascii_start();
        let end_of_inode = tail.iter().position(|byte| *byte == b' ')?;
        let path = tail[end_of_inode..].trim_ascii_start();
        let path = Path::new(std::ffi::OsStr::from_bytes(path));
        (path.is_absolute()
            && path
                .file_name()
                .is_some_and(|name| name == "SingletonSocket"))
        .then_some(path)
    })
}

fn connect_browser(socket: &Path, pid: u32) -> Option<UnixStream> {
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    if std::fs::metadata(socket).ok()?.uid() != uid {
        return None;
    }
    let address = SockAddr::unix(socket).ok()?;
    let connection = Socket::new(Domain::UNIX, Type::STREAM, None).ok()?;
    // A full Unix-socket backlog can block connect before the read/write
    // timeouts apply. Skip sockets that cannot accept a connection now.
    connection.set_nonblocking(true).ok()?;
    connection.connect(&address).ok()?;
    connection.set_nonblocking(false).ok()?;
    let stream: UnixStream = connection.into();
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: credentials has space for ucred. On success getsockopt
    // initializes the returned number of bytes.
    let peer = unsafe {
        (libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut size,
        ) == 0
            && size as usize == std::mem::size_of::<libc::ucred>())
        .then(|| credentials.assume_init())
    };
    // Hyprland supplies the PID of the selected Chromium window.
    peer.is_some_and(|peer| peer.pid as u32 == pid && peer.uid == uid)
        .then_some(stream)
}

fn browser_stream(pid: u32) -> Result<UnixStream, &'static str> {
    const ERROR: &str = "workspace_browser_unavailable";
    // The browser can choose any profile directory and rewrite its own
    // command line. Locate the listening endpoint the kernel reports.
    let table = std::fs::read("/proc/net/unix").map_err(|_| ERROR)?;
    singleton_paths(&table)
        .find_map(|socket| connect_browser(socket, pid))
        .ok_or(ERROR)
}

// Chromium's POSIX ProcessSingleton accepts a NUL-delimited command line.
fn send_browser_link(mut stream: UnixStream, url: &str) -> Result<(), &'static str> {
    const ERROR: &str = "workspace_browser_unavailable";
    const TIMEOUT: Option<Duration> = Some(Duration::from_secs(2));
    let mut message = b"START\0/\0browser\0--\0".to_vec();
    message.extend_from_slice(url.as_bytes());
    if message.len() > 32 * 1024 {
        return Err(ERROR);
    }
    stream.set_write_timeout(TIMEOUT).map_err(|_| ERROR)?;
    stream.set_read_timeout(TIMEOUT).map_err(|_| ERROR)?;
    stream.write_all(&message).map_err(|_| ERROR)?;
    stream.shutdown(Shutdown::Write).map_err(|_| ERROR)?;
    let mut reply = [0; 3];
    stream.read_exact(&mut reply).map_err(|_| ERROR)?;
    if &reply != b"ACK" {
        return Err(ERROR);
    }
    Ok(())
}

fn window_pid(window: &Value) -> Result<u32, &'static str> {
    window["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or("workspace_browser_unavailable")
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

fn deliver_link(window: &Value, url: &str) -> Result<(), &'static str> {
    let stream = browser_stream(window_pid(window)?)?;
    focus_browser(window)?;
    send_browser_link(stream, url)
}

fn mail_in_workspace(clients: &[Value]) -> bool {
    clients
        .iter()
        .any(|client| is_mail(client) && belongs(client))
}

fn release_window(window: &Value, destination: Option<&str>) -> Result<(), &'static str> {
    if let Some(destination) = destination {
        return dispatch(&[move_window(window, destination)?]);
    }
    let monitors = query("monitors")?;
    let workspace = regular_workspace(array(&monitors)?).ok_or("workspace_unavailable")?;
    dispatch(&[move_window(window, &workspace)?])
}

fn link_visibility(
    generation: u64,
) -> Result<std::sync::MutexGuard<'static, Visibility>, &'static str> {
    let visibility = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
    if visibility.dismissal_generation != generation {
        return Err("workspace_browser_unavailable");
    }
    if QUITTING.load(Ordering::Relaxed) {
        return Err("workspace_unavailable");
    }
    Ok(visibility)
}

pub(super) fn open_link(url: &str) -> Result<bool, &'static str> {
    if !valid_link(url) {
        return Err("workspace_invalid_link");
    }
    let dismissal_generation = {
        let visibility = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
        if visibility.dismissed {
            return Err("workspace_browser_unavailable");
        }
        visibility.dismissal_generation
    };
    let _launch = BROWSER_LAUNCH.lock().map_err(|_| "workspace_unavailable")?;
    if QUITTING.load(Ordering::Relaxed) {
        return Err("workspace_unavailable");
    }
    if !mail_in_workspace(array(&query("clients")?)?) {
        let _visibility = link_visibility(dismissal_generation)?;
        return Ok(false);
    }
    let desktop = output("omarchy-cmd-default-browser", &[])
        .or_else(|_| output("xdg-settings", &["get", "default-web-browser"]))?;
    let (companion, previous) = {
        // Keep the dismissal check and the launch under the same lock. The
        // default-browser lookup and the wait for a new window run outside it.
        let _visibility = link_visibility(dismissal_generation)?;
        let current = query("clients")?;
        let current = array(&current)?;
        if !mail_in_workspace(current) {
            return Ok(false);
        }
        let Some(browser) = BROWSERS
            .iter()
            .find(|browser| browser.desktop == desktop.trim())
        else {
            return Ok(false);
        };
        let previous_companion = BROWSER.lock().map_err(|_| "workspace_unavailable")?.clone();
        if let Some(previous) = previous_companion
            && let Some(window) = current
                .iter()
                .find(|client| belongs(client) && previous.matches(client))
        {
            if previous.desktop == browser.desktop {
                // Chromium uses the profile of the window that takes focus.
                deliver_link(window, url)?;
                return Ok(true);
            }
            release_window(window, None)?;
            *BROWSER.lock().map_err(|_| "workspace_unavailable")? = None;
        }
        let companion = Companion {
            desktop: browser.desktop,
            title: browser_title()?,
        };
        let previous = current
            .iter()
            .filter_map(address)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        browser_spawn(browser, &companion.title)?;
        (companion, previous)
    };
    let window = wait_client(
        |client| {
            companion.matches(client)
                && !address(client).is_some_and(|a| previous.iter().any(|old| old == a))
        },
        7,
    )?;
    let visibility = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
    let accompany =
        !QUITTING.load(Ordering::Relaxed) && mail_in_workspace(array(&query("clients")?)?);
    if accompany {
        *BROWSER.lock().map_err(|_| "workspace_unavailable")? = Some(companion);
        place(&window)?;
    } else if belongs(&window) {
        release_window(&window, visibility.destination.as_deref())?;
    }
    if visibility.dismissal_generation != dismissal_generation
        || (accompany && visible_monitor(array(&query("monitors")?)?).is_none())
    {
        // A dismissal while the browser was starting wins over taking focus.
        return Err("workspace_browser_unavailable");
    }
    deliver_link(&window, url)?;
    Ok(true)
}

pub(super) fn release() -> Result<bool, &'static str> {
    QUITTING.store(true, Ordering::Relaxed);
    // Let an in-flight browser window finish mapping before quitting. Never
    // take this lock inside the visibility lock used by present and hide.
    let _launch = BROWSER_LAUNCH.lock().map_err(|_| "workspace_unavailable")?;
    let visibility = VISIBILITY.lock().map_err(|_| "workspace_unavailable")?;
    release_companion(visibility.destination.as_deref())
}

fn release_companion(destination: Option<&str>) -> Result<bool, &'static str> {
    let mut own = BROWSER.lock().map_err(|_| "workspace_unavailable")?;
    let Some(companion) = own.as_ref() else {
        return Ok(false);
    };
    let clients = query("clients")?;
    if let Some(window) = array(&clients)?
        .iter()
        .find(|client| belongs(client) && companion.matches(client))
    {
        release_window(window, destination)?;
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
    fn regular_workspace_selector_handles_named_and_numbered_workspaces() {
        assert_eq!(
            workspace_selector(&serde_json::json!({"id": 4, "name": "Mail"})).as_deref(),
            Some("4")
        );
        assert_eq!(
            workspace_selector(&serde_json::json!({"id": -1337, "name": "Boîte \"Mail\""}))
                .as_deref(),
            Some(r#"name:Boîte "Mail""#)
        );
        assert_eq!(
            workspace_selector(&serde_json::json!({"id": -99, "name": "special:notes"})),
            None
        );
        assert_eq!(
            workspace_selector(&serde_json::json!({"id": 0, "name": ""})),
            None
        );
    }

    #[test]
    fn named_workspace_cannot_escape_the_lua_string() {
        let client = serde_json::json!({"address": "0x1"});
        assert_eq!(
            move_window(&client, r#"name:A "quoted" \ name"#).unwrap(),
            r#"hl.dsp.window.move({ workspace = "name:A \"quoted\" \\ name", follow = false, window = "address:0x1" })"#
        );
    }

    #[test]
    fn singleton_paths_only_select_listeners_and_preserve_spaces() {
        let sockets =
            b"000000: 00000002 00000000 00010000 0001 01    42 /tmp/a b/SingletonSocket\n\
                        000000: 00000002 00000000 00000000 0001 03 12345 /tmp/c/SingletonSocket\n\
                        000000: 00000002 00000000 00010000 0001 01 12346 /tmp/other";
        assert_eq!(
            singleton_paths(sockets).collect::<Vec<_>>(),
            [Path::new("/tmp/a b/SingletonSocket")]
        );
    }

    #[test]
    fn companion_identity_survives_navigation_away_from_the_bootstrap_tab() {
        let browser = Companion {
            title: "Omamail probe".into(),
            desktop: "brave-browser.desktop",
        };
        assert!(browser.matches(&serde_json::json!({
            "address": "0x1",
            "initialTitle": "Omamail probe - Brave",
            "title": "Read more - Brave"
        })));
        assert!(!browser.matches(&serde_json::json!({
            "address": "0x2",
            "initialTitle": "Omamail probe2 - Brave",
            "title": "Read more - Brave"
        })));
    }

    #[test]
    fn browser_url_is_sent_to_the_verified_socket() {
        use std::os::unix::net::UnixListener;

        let directory = std::env::temp_dir().join(browser_title().unwrap());
        std::fs::create_dir(&directory).unwrap();
        let socket = directory.join("SingletonSocket");
        let listener = UnixListener::bind(&socket).unwrap();
        let receiver = std::thread::spawn(move || {
            let (mut rejected, _) = listener.accept().unwrap();
            assert_eq!(rejected.read(&mut [0]).unwrap(), 0);

            let (mut accepted, _) = listener.accept().unwrap();
            let mut message = Vec::new();
            accepted.read_to_end(&mut message).unwrap();
            accepted.write_all(b"ACK").unwrap();
            message
        });
        let url = "https://example.org/reset?token=synthetic-test";
        assert!(connect_browser(&socket, 0).is_none());
        let stream = connect_browser(&socket, std::process::id()).unwrap();
        send_browser_link(stream, url).unwrap();
        let received = receiver.join().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(
            received
                .split(|byte| *byte == 0)
                .skip(2)
                .collect::<Vec<_>>(),
            [b"browser".as_slice(), b"--", url.as_bytes()]
        );
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
