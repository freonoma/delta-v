use std::{
    env,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::{credentials, model::ProviderId};

use super::auth::Error;

const SETTINGS: &str = r#"{"disableAllHooks":true,"disableRemoteControl":true,"disableAgentView":true,"disableClaudeAiConnectors":true,"disableDeepLinkRegistration":"disable"}"#;
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(100);
const READ_INTERVAL: Duration = Duration::from_secs(1);
const DEADLINE: Duration = Duration::from_secs(120);
const EXIT_GRACE: Duration = Duration::from_secs(45);

pub(super) fn renew(executable: &Path, cwd: &Path, cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    if !credentials::claude_allows_renewal().map_err(|_| Error::ManualSignInRequired)? {
        return Err(Error::ManualSignInRequired);
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(Error::ManualSignInRequired)?;
    let config = config_directory("CLAUDE_CONFIG_DIR", &home.join(".claude"))?;
    let secure = config_directory("CLAUDE_SECURESTORAGE_CONFIG_DIR", &config)?;
    managed_guard(&config)?;
    let initial = credentials::load_sync(ProviderId::Claude).map_err(|_| Error::Failed)?;
    let search_path = super::auth::claude_search_path(cwd)?;
    let command = command(executable, cwd, &search_path);
    run(
        command,
        cancel,
        DEADLINE,
        EXIT_GRACE,
        &refresh_locks(&secure),
        || {
            credentials::load_sync(ProviderId::Claude)
                .map(|current| current != initial)
                .unwrap_or(false)
        },
    )
}

fn config_directory(key: &str, fallback: &Path) -> Result<PathBuf, Error> {
    match env::var(key) {
        Ok(value) if value.is_empty() => Err(Error::ManualSignInRequired),
        Ok(value) => {
            let path = PathBuf::from(super::normalize_nfc(&value));
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(Error::ManualSignInRequired)
            }
        }
        Err(env::VarError::NotPresent) => Ok(fallback.to_path_buf()),
        Err(_) => Err(Error::ManualSignInRequired),
    }
}

fn managed_guard(config: &Path) -> Result<(), Error> {
    let output =
        metadata_command("/usr/bin/id", &["-un"], 256).ok_or(Error::ManualSignInRequired)?;
    let user = std::str::from_utf8(&output)
        .map_err(|_| Error::ManualSignInRequired)?
        .trim();
    if user.is_empty() || user.contains('/') {
        return Err(Error::ManualSignInRequired);
    }
    let paths = [
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json"),
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.d"),
        PathBuf::from("/Library/Managed Preferences/com.anthropic.claudecode.plist"),
        PathBuf::from("/Library/Managed Preferences")
            .join(user)
            .join("com.anthropic.claudecode.plist"),
        config.join("remote-settings.json"),
    ];
    if paths.iter().any(|path| present_or_unreadable(path)) {
        return Err(Error::ManualSignInRequired);
    }
    Ok(())
}

fn present_or_unreadable(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

fn refresh_locks(directory: &Path) -> Vec<PathBuf> {
    let canonical = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    let mut legacy = canonical.as_os_str().to_os_string();
    legacy.push(".lock");
    vec![directory.join(".oauth_refresh.lock"), PathBuf::from(legacy)]
}

fn command(executable: &Path, cwd: &Path, search_path: &OsStr) -> Command {
    let mut command = Command::new("/usr/bin/script");
    command
        .args(["-q", "/dev/null"])
        .arg(executable)
        .args([
            "--safe-mode",
            "--restricted",
            "--setting-sources=",
            "--settings",
            SETTINGS,
            "--tools",
            "",
            "--strict-mcp-config",
            "--mcp-config",
            r#"{"mcpServers":{}}"#,
            "--permission-mode",
            "dontAsk",
            "--no-chrome",
            "/status",
        ])
        .current_dir(cwd)
        .env_clear();
    for key in [
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    ] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("PATH", search_path)
        .env("TERM", "xterm-256color")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("DISABLE_ERROR_REPORTING", "1")
        .env("ENABLE_CLAUDEAI_MCP_SERVERS", "false")
        .env("CLAUDE_CODE_DISABLE_AGENT_VIEW", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

fn run(
    mut command: Command,
    cancel: &AtomicBool,
    deadline: Duration,
    exit_grace: Duration,
    locks: &[PathBuf],
    mut changed: impl FnMut() -> bool,
) -> Result<(), Error> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let mut child = command.spawn().map_err(|_| Error::Failed)?;
    let mut input = child.stdin.take();
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let group = direct_child_group(child.id());
        stop(&mut child, &mut input, group, exit_grace, locks, || {});
        return Err(Error::Failed);
    };
    let mut output = Output::default();
    let readable = nonblocking(&stdout).and_then(|()| nonblocking(&stderr));
    let mut group = None;
    let started = Instant::now();
    let mut next_read = started + READ_INTERVAL;
    let outcome = loop {
        if group.is_none() {
            group = direct_child_group(child.id());
        }
        if cancel.load(Ordering::Acquire) {
            break Err(Error::Cancelled);
        }
        if readable.is_err() || !output.drain(&mut stdout) || !output.drain(&mut stderr) {
            break Err(Error::Failed);
        }
        if Instant::now() >= next_read {
            if changed() {
                break Ok(());
            }
            next_read = Instant::now() + READ_INTERVAL;
        }
        match child.try_wait() {
            Ok(Some(_)) => {
                break if changed() {
                    Ok(())
                } else {
                    Err(Error::Failed)
                };
            }
            Ok(None) => {}
            Err(_) => break Err(Error::Failed),
        }
        if started.elapsed() >= deadline {
            break Err(Error::TimedOut);
        }
        thread::sleep(POLL);
    };
    stop(&mut child, &mut input, group, exit_grace, locks, || {
        if readable.is_ok() {
            let _ = output.drain(&mut stdout);
            let _ = output.drain(&mut stderr);
        }
    });
    outcome
}

fn nonblocking(source: &impl AsRawFd) -> std::io::Result<()> {
    unsafe extern "C" {
        fn fcntl(fd: i32, command: i32, ...) -> i32;
    }
    let fd = source.as_raw_fd();
    let flags = unsafe { fcntl(fd, 3) };
    if flags < 0 || unsafe { fcntl(fd, 4, flags | 0x0004) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Default)]
struct Output {
    count: usize,
    screen: Screen,
}

impl Output {
    fn drain(&mut self, source: &mut impl Read) -> bool {
        let mut buffer = [0_u8; 4096];
        for _ in 0..32 {
            match source.read(&mut buffer) {
                Ok(0) => return true,
                Ok(count) => {
                    self.count = self.count.saturating_add(count);
                    if self.count > MAX_OUTPUT || self.screen.push(&buffer[..count]) {
                        return false;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return true,
                Err(_) => return false,
            }
        }
        true
    }
}

// `script` gives its child a new terminal session, outside the wrapper's process group.
fn direct_child_group(parent: u32) -> Option<u32> {
    let output = metadata_command("/bin/ps", &["-axo", "pid=,ppid=,pgid="], 512 * 1024)?;
    child_group(&String::from_utf8_lossy(&output), parent)
}

fn metadata_command(program: &str, args: &[&str], cap: usize) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let result = (|| {
        let mut pipe = child.stdout.take()?;
        nonblocking(&pipe).ok()?;
        let started = Instant::now();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    if let Some(status) = child.try_wait().ok()? {
                        return status.success().then_some(bytes);
                    }
                }
                Ok(count) => {
                    if bytes.len() + count > cap {
                        return None;
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
            if started.elapsed() >= Duration::from_secs(2) {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

fn child_group(table: &str, parent: u32) -> Option<u32> {
    table.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.parse::<u32>().ok()?;
        let ppid = fields.next()?.parse::<u32>().ok()?;
        let pgid = fields.next()?.parse::<u32>().ok()?;
        (ppid == parent && pid == pgid && pid > 1 && pid <= i32::MAX as u32).then_some(pid)
    })
}

fn stop(
    child: &mut Child,
    input: &mut Option<ChildStdin>,
    group: Option<u32>,
    grace: Duration,
    locks: &[PathBuf],
    mut drain: impl FnMut(),
) {
    // Claude's own shutdown waits only two seconds for a refresh already in flight.
    let started = Instant::now();
    while locks.iter().any(|path| present_or_unreadable(path)) && started.elapsed() < grace {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        drain();
        thread::sleep(POLL);
    }
    if let Some(input) = input.as_mut() {
        let _ = input.write_all(b"\x03");
        let _ = input.flush();
        thread::sleep(Duration::from_millis(250));
        let _ = input.write_all(b"\x03\x04");
        let _ = input.flush();
    }
    input.take();
    let group = group.or_else(|| direct_child_group(child.id()));
    if wait(child, group, Duration::from_secs(5), &mut drain) {
        return;
    }
    if let Some(group) = group {
        signal_group(group, 15);
    }
    if !wait(child, group, Duration::from_secs(5), &mut drain) {
        if let Some(group) = group {
            signal_group(group, 9);
        }
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn wait(
    child: &mut Child,
    group: Option<u32>,
    duration: Duration,
    drain: &mut impl FnMut(),
) -> bool {
    let started = Instant::now();
    while started.elapsed() < duration {
        drain();
        if matches!(child.try_wait(), Ok(Some(_))) && !group.is_some_and(group_alive) {
            return true;
        }
        thread::sleep(POLL);
    }
    false
}

fn group_alive(group: u32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    unsafe { kill(-(group as i32), 0) == 0 }
}

fn signal_group(group: u32, signal: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    unsafe {
        kill(-(group as i32), signal);
    }
}

#[derive(Default)]
struct Screen {
    text: String,
    escape: u8,
}

impl Screen {
    fn push(&mut self, bytes: &[u8]) -> bool {
        for &byte in bytes {
            match self.escape {
                1 => {
                    self.escape = match byte {
                        b'[' => 2,
                        b']' => 3,
                        _ => 0,
                    }
                }
                2 => {
                    if (0x40..=0x7e).contains(&byte) {
                        self.escape = 0;
                    }
                }
                3 => match byte {
                    7 => self.escape = 0,
                    27 => self.escape = 4,
                    _ => {}
                },
                4 => self.escape = if byte == b'\\' { 0 } else { 3 },
                _ => match byte {
                    27 => self.escape = 1,
                    8 => {
                        self.text.pop();
                    }
                    b'\n' | b'\r' => self.text.push('\n'),
                    0x20..=0x7e => self.text.push(char::from(byte.to_ascii_lowercase())),
                    _ => {}
                },
            }
        }
        if self.text.len() > 8192 {
            self.text.drain(..self.text.len() - 8192);
        }
        [
            "trust this folder",
            "quick safety check",
            "do you trust the files",
            "select login method",
            "choose the text style",
            "do you want to proceed",
            "allow claude to",
            "not logged in",
            "please run /login",
            "unknown option",
        ]
        .iter()
        .any(|needle| self.text.contains(needle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, sync::atomic::AtomicU64};

    struct FakeClient(PathBuf);

    impl FakeClient {
        fn new(body: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let directory = env::temp_dir().join(format!(
                "delta-v-claude-auth-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let executable = directory.join("fake-client");
            fs::write(
                &executable,
                format!("#!/bin/sh\nprintf '%s' \"$$\" > helper-pid\n{body}\n"),
            )
            .unwrap();
            fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self(directory)
        }

        fn command(&self) -> Command {
            command(
                &self.0.join("fake-client"),
                &self.0,
                OsStr::new("/bin:/usr/sbin:/sbin"),
            )
        }

        fn assert_stopped(&self) {
            let pid: u32 = fs::read_to_string(self.0.join("helper-pid"))
                .unwrap()
                .parse()
                .unwrap();
            assert!(
                !group_alive(pid),
                "the helper process group survived cleanup"
            );
        }
    }

    impl Drop for FakeClient {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn clean_client_exit_without_changed_credentials_is_failure() {
        let client = FakeClient::new("exit 0");
        let result = run(
            client.command(),
            &AtomicBool::new(false),
            Duration::from_secs(3),
            Duration::from_millis(100),
            &[],
            || false,
        );
        assert_eq!(result, Err(Error::Failed));
        client.assert_stopped();
    }

    #[test]
    fn saved_credential_change_is_success_and_the_helper_stops() {
        let client = FakeClient::new("trap 'exit 0' INT HUP TERM\nwhile :; do /bin/sleep 1; done");
        let result = run(
            client.command(),
            &AtomicBool::new(false),
            Duration::from_secs(3),
            Duration::from_millis(100),
            &[],
            || true,
        );
        assert_eq!(result, Ok(()));
        client.assert_stopped();
    }

    #[test]
    fn approval_is_never_accepted() {
        let client = FakeClient::new(
            "printf 'Do you trust the files?'\ntrap 'exit 0' INT HUP TERM\nwhile :; do /bin/sleep 1; done",
        );
        let result = run(
            client.command(),
            &AtomicBool::new(false),
            Duration::from_secs(3),
            Duration::from_millis(100),
            &[],
            || false,
        );
        assert_eq!(result, Err(Error::Failed));
        client.assert_stopped();
    }

    #[test]
    fn cancellation_waits_for_a_refresh_lock_to_clear() {
        let client = FakeClient::new("trap 'exit 0' INT HUP TERM\nwhile :; do /bin/sleep 1; done");
        let lock = client.0.join(".oauth_refresh.lock");
        fs::create_dir(&lock).unwrap();
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        thread::scope(|scope| {
            let lock = &lock;
            let cancel = &cancel;
            let marker = client.0.join("helper-pid");
            scope.spawn(move || {
                let waiting = Instant::now();
                while fs::read_to_string(&marker)
                    .ok()
                    .and_then(|pid| pid.parse::<u32>().ok())
                    .is_none()
                {
                    assert!(
                        waiting.elapsed() < Duration::from_secs(3),
                        "fake client did not start"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                cancel.store(true, Ordering::Release);
                thread::sleep(Duration::from_millis(400));
                fs::remove_dir(lock).unwrap();
            });
            let result = run(
                client.command(),
                cancel,
                Duration::from_secs(3),
                Duration::from_secs(2),
                std::slice::from_ref(lock),
                || false,
            );
            assert_eq!(result, Err(Error::Cancelled));
        });
        assert!(started.elapsed() >= Duration::from_millis(400));
        client.assert_stopped();
    }

    #[test]
    fn only_the_scripts_direct_child_can_be_signalled() {
        let table = "10 1 10\n11 10 11\n12 1 12\n13 11 11\n";
        assert_eq!(child_group(table, 10), Some(11));
        assert_eq!(child_group(table, 13), None);
        assert_eq!(child_group("11 10 10\n", 10), None);
        assert_eq!(child_group("1 10 1\n", 10), None);
    }

    #[test]
    fn terminal_controls_do_not_hide_a_split_approval_prompt() {
        let mut screen = Screen::default();
        assert!(!screen.push(b"\x1b[31mDo you tru"));
        assert!(screen.push(b"st the files\x1b[0m?"));
        let mut screen = Screen::default();
        assert!(!screen.push(b"\x1b]0;private terminal title\x07Version: 2.1.270"));
        assert!(!screen.text.contains("private"));
    }

    #[test]
    fn terminal_history_is_bounded() {
        let mut screen = Screen::default();
        assert!(!screen.push(&vec![b'x'; 50_000]));
        assert_eq!(screen.text.len(), 8192);
    }

    #[test]
    fn the_only_submitted_command_is_status_and_no_shell_interprets_it() {
        let command = command(
            Path::new("/test/claude"),
            Path::new("/test"),
            OsStr::new("/test/bin:/bin:/usr/sbin:/sbin"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert_eq!(command.get_program(), "/usr/bin/script");
        assert_eq!(&args[..3], ["-q", "/dev/null", "/test/claude"]);
        assert_eq!(args.last().unwrap(), "/status");
        assert!(args.windows(2).any(|pair| pair == ["--tools", ""]));
        assert!(args.contains(&"--safe-mode".into()));
        assert!(!args.contains(&"--print".into()));
        assert!(!args.contains(&"/usage".into()));
    }
}
