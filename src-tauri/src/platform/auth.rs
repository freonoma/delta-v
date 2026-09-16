use std::{
    env,
    ffi::OsString,
    fs::{self, DirBuilder},
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::{fs::DirBuilderExt, fs::PermissionsExt, fs::symlink, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::ProviderId;

const MAX_OUTPUT: usize = 512 * 1024;
const MAX_MESSAGES: usize = 256;
const POLL: Duration = Duration::from_millis(50);
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
const O_NONBLOCK: i32 = 0x0004;
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const CODEX_SETTINGS: &[&str] = &[
    "analytics.enabled=false",
    "feedback.enabled=false",
    "otel.exporter=\"none\"",
    "otel.trace_exporter=\"none\"",
    "otel.metrics_exporter=\"none\"",
    "features.hooks=false",
    "features.plugins=false",
    "features.apps=false",
    "features.code_mode_host=false",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Renew,
    SignIn,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("Install or update {0}, then try again.")]
    ClientMissing(&'static str),
    #[error("Sign in to Codex with a ChatGPT account to read subscription usage.")]
    UnsupportedAccount,
    #[error(
        "This Claude setup needs reconnecting in Claude Code. Run /status there, or /login if asked, then choose Check now."
    )]
    ManualSignInRequired,
    #[error("The official client could not complete sign-in. Open it and try again.")]
    Failed,
    #[error("Connection was cancelled.")]
    Cancelled,
    #[error("The official client did not finish in time. Open it and try again.")]
    TimedOut,
    #[error("The official client returned an unsupported response. Update it and try again.")]
    Protocol,
}

pub fn recover(provider: ProviderId, action: Action, cancel: Arc<AtomicBool>) -> Result<(), Error> {
    if cancel.load(Ordering::Acquire) {
        return Err(Error::Cancelled);
    }
    let directory = ScratchDirectory::create()?;
    let executable = resolve_client(provider, &directory.0, &cancel)?;
    match provider {
        ProviderId::Codex => recover_codex(&executable, &directory.0, action, &cancel),
        ProviderId::Claude => match action {
            Action::Renew => super::claude_auth::renew(&executable, &directory.0, &cancel),
            Action::SignIn => sign_in_claude(&executable, &directory.0, &cancel),
        },
    }
}

fn client_name(provider: ProviderId) -> &'static str {
    match provider {
        ProviderId::Claude => "Claude Code 2.1.270 or newer",
        ProviderId::Codex => "Codex 0.154 or newer",
    }
}

fn resolve_client(
    provider: ProviderId,
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, Error> {
    let (name, override_key, bundles): (&str, &str, &[&str]) = match provider {
        ProviderId::Codex => (
            "codex",
            "DELTA_V_CODEX_PATH",
            &[
                "/Applications/ChatGPT.app/Contents/Resources/codex",
                "/Applications/Codex.app/Contents/Resources/codex",
            ],
        ),
        ProviderId::Claude => ("claude", "DELTA_V_CLAUDE_PATH", &[]),
    };
    if let Some(path) = env::var_os(override_key) {
        let path =
            executable_path(Path::new(&path)).ok_or(Error::ClientMissing(client_name(provider)))?;
        check_version(provider, &path, directory, cancel)?;
        return Ok(path);
    }
    let mut candidates: Vec<PathBuf> = bundles.iter().map(PathBuf::from).collect();
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/bin").join(name));
        candidates.push(home.join(".npm-global/bin").join(name));
        if provider == ProviderId::Codex {
            candidates.push(home.join("Applications/ChatGPT.app/Contents/Resources/codex"));
            candidates.push(home.join("Applications/Codex.app/Contents/Resources/codex"));
        }
    }
    candidates.push(Path::new("/opt/homebrew/bin").join(name));
    candidates.push(Path::new("/usr/local/bin").join(name));
    if let Some(path) = env::var_os("PATH") {
        candidates.extend(
            env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(name)),
        );
    }
    let started = Instant::now();
    let mut checked = Vec::new();
    for candidate in candidates {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if started.elapsed() >= Duration::from_secs(20) {
            break;
        }
        let Some(path) = executable_path(&candidate) else {
            continue;
        };
        if checked.contains(&path) {
            continue;
        }
        checked.push(path.clone());
        match check_version(provider, &path, directory, cancel) {
            Ok(()) => return Ok(path),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(Error::ManualSignInRequired) => return Err(Error::ManualSignInRequired),
            Err(_) => {}
        }
    }
    Err(Error::ClientMissing(client_name(provider)))
}

fn executable_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return None;
    }
    path.canonicalize().ok()
}

fn base_command(
    provider: ProviderId,
    executable: &Path,
    directory: &Path,
) -> Result<Command, Error> {
    let mut command = Command::new(executable);
    command.current_dir(directory).env_clear();
    for key in ["HOME", "USER", "LOGNAME", "TMPDIR", "LANG", "LC_ALL"] {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
    match provider {
        ProviderId::Codex => {
            command.env("PATH", absolute_search_path());
            if let Some(home) = env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
                let path = PathBuf::from(home);
                let absolute = if path.is_absolute() {
                    path
                } else {
                    env::current_dir().map_err(|_| Error::Failed)?.join(path)
                };
                command.env("CODEX_HOME", absolute);
            }
            command
                .env("CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED", "1")
                .env("OTEL_SDK_DISABLED", "true");
            for setting in CODEX_SETTINGS {
                command.args(["-c", setting]);
            }
        }
        ProviderId::Claude => {
            command.env("PATH", claude_search_path(directory)?);
            for key in ["CLAUDE_CONFIG_DIR", "CLAUDE_SECURESTORAGE_CONFIG_DIR"] {
                if let Some(value) = env::var_os(key) {
                    // Rewriting the path changes Claude's Keychain service hash.
                    if value.is_empty() || !Path::new(&value).is_absolute() {
                        return Err(Error::ManualSignInRequired);
                    }
                    command.env(key, value);
                }
            }
            command
                .env("DISABLE_AUTOUPDATER", "1")
                .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
                .env("DISABLE_TELEMETRY", "1")
                .env("DISABLE_ERROR_REPORTING", "1")
                .env("ENABLE_CLAUDEAI_MCP_SERVERS", "false")
                .env("CLAUDE_CODE_DISABLE_AGENT_VIEW", "1");
        }
    }
    Ok(command)
}

fn absolute_search_path() -> OsString {
    let mut paths: Vec<PathBuf> = env::var_os("PATH")
        .map(|value| {
            env::split_paths(&value)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    paths.extend(
        [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ]
        .map(PathBuf::from),
    );
    env::join_paths(paths).unwrap_or_else(|_| OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"))
}

pub(super) fn claude_search_path(directory: &Path) -> Result<OsString, Error> {
    let bin = directory.join("auth-bin");
    match DirBuilder::new().mode(0o700).create(&bin) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Failed),
    }
    let mut programs = vec![
        ("security", PathBuf::from("/usr/bin/security")),
        ("open", PathBuf::from("/usr/bin/open")),
    ];
    if let Some(node) = env::split_paths(&absolute_search_path())
        .find_map(|path| executable_path(&path.join("node")))
    {
        programs.push(("node", node));
    }
    for (name, executable) in programs {
        let link = bin.join(name);
        match symlink(&executable, &link) {
            Ok(()) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
                    && fs::read_link(&link).ok().as_ref() == Some(&executable) => {}
            Err(_) => return Err(Error::Failed),
        }
    }
    env::join_paths([
        bin,
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ])
    .map_err(|_| Error::Failed)
}

fn check_version(
    provider: ProviderId,
    executable: &Path,
    directory: &Path,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    let mut command = base_command(provider, executable, directory)?;
    command.arg("--version");
    let mut process = ClientProcess::spawn(command, 8192)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = (|| {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            process.receive_output(POLL)?;
            if let Some(status) = process.child.try_wait().map_err(|_| Error::Failed)? {
                process.drain_to_eof(deadline)?;
                let text = std::str::from_utf8(&process.buffer).map_err(|_| Error::Protocol)?;
                if status.success() && supported_version(provider, text) {
                    return Ok(());
                }
                return Err(Error::ClientMissing(client_name(provider)));
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
        }
    })();
    process.finish();
    result
}

fn supported_version(provider: ProviderId, text: &str) -> bool {
    let version = text.split_whitespace().find_map(|part| {
        let part = part.strip_prefix('v').unwrap_or(part);
        let mut numbers = part.split(['.', '-']);
        let major = numbers.next()?.parse::<u32>().ok()?;
        let minor = numbers.next()?.parse::<u32>().ok()?;
        let patch = numbers.next()?.parse::<u32>().ok()?;
        Some((major, minor, patch))
    });
    let minimum = match provider {
        ProviderId::Codex => (0, 154, 0),
        ProviderId::Claude => (2, 1, 270),
    };
    version.is_some_and(|version| version >= minimum)
}

fn recover_codex(
    executable: &Path,
    directory: &Path,
    action: Action,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    let mut command = base_command(ProviderId::Codex, executable, directory)?;
    command.args(["app-server", "--listen", "stdio://"]);
    let mut process = ClientProcess::spawn(command, MAX_OUTPUT)?;
    let deadline =
        Instant::now() + Duration::from_secs(if action == Action::Renew { 120 } else { 180 });
    let result = (|| {
        process.send(&json!({
            "id": 1, "method": "initialize", "params": {
                "clientInfo": {"name": "delta_v", "title": "Delta-V", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": false}
            }
        }))?;
        wait_response(&mut process, 1, deadline, Some(cancel))?;
        process.send(&json!({"method": "initialized", "params": {}}))?;
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        match action {
            Action::Renew => {
                process.send(
                    &json!({"id": 2, "method": "account/read", "params": {"refreshToken": true}}),
                )?;
                // Closing a client mid-rotation can strand the credential it owns.
                let response = wait_response(&mut process, 2, deadline, None)?;
                if cancel.load(Ordering::Acquire) {
                    return Err(Error::Cancelled);
                }
                if response
                    .get("account")
                    .and_then(|a| a.get("type"))
                    .and_then(Value::as_str)
                    != Some("chatgpt")
                {
                    return Err(Error::UnsupportedAccount);
                }
                // The caller verifies fresh usage because account/read can swallow renewal errors.
                Ok(())
            }
            Action::SignIn => codex_sign_in(&mut process, directory, deadline, cancel),
        }
    })();
    process.finish();
    result
}

fn codex_sign_in(
    process: &mut ClientProcess,
    directory: &Path,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    process
        .send(&json!({"id": 2, "method": "account/login/start", "params": {"type": "chatgpt"}}))?;
    let response = wait_response(process, 2, deadline, Some(cancel))?;
    if response.get("type").and_then(Value::as_str) != Some("chatgpt") {
        return Err(Error::Protocol);
    }
    let login_id = response
        .get("loginId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or(Error::Protocol)?;
    let url = response
        .get("authUrl")
        .and_then(Value::as_str)
        .ok_or(Error::Protocol)?;
    let result = (|| {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        open_auth_url(url, directory, cancel)?;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let Some(message) = process.next_message(deadline)? else {
                continue;
            };
            if message.get("id").is_some() {
                return Err(Error::Protocol);
            }
            if message.get("method").and_then(Value::as_str) == Some("account/login/completed") {
                let params = message.get("params").ok_or(Error::Protocol)?;
                if params.get("loginId").and_then(Value::as_str) != Some(login_id) {
                    return Err(Error::Protocol);
                }
                return match params.get("success").and_then(Value::as_bool) {
                    Some(true) => Ok(()),
                    Some(false) => Err(Error::Failed),
                    None => Err(Error::Protocol),
                };
            }
        }
    })();
    if result.is_err() {
        let _ = cancel_codex_login(process, login_id);
    }
    result
}

fn cancel_codex_login(process: &mut ClientProcess, login_id: &str) -> Result<(), Error> {
    process.send(
        &json!({"id": 3, "method": "account/login/cancel", "params": {"loginId": login_id}}),
    )?;
    let _ = wait_response(process, 3, Instant::now() + Duration::from_secs(5), None);
    Err(Error::Cancelled)
}

fn valid_auth_url(text: &str) -> bool {
    if text.len() > 16 * 1024 {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(text) else {
        return false;
    };
    url.scheme() == "https"
        && matches!(url.host_str(), Some("auth.openai.com" | "chatgpt.com"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.fragment().is_none()
}

fn open_auth_url(url: &str, directory: &Path, cancel: &AtomicBool) -> Result<(), Error> {
    if !valid_auth_url(url) {
        return Err(Error::Protocol);
    }
    let mut command = Command::new("/usr/bin/open");
    command.arg(url).current_dir(directory);
    let mut process = ClientProcess::spawn(command, 8192)?;
    let result = process.wait_exit(Instant::now() + Duration::from_secs(10), cancel);
    process.finish();
    result
}

fn sign_in_claude(executable: &Path, directory: &Path, cancel: &AtomicBool) -> Result<(), Error> {
    let mut command = base_command(ProviderId::Claude, executable, directory)?;
    command.args([
        "--safe-mode", "--restricted", "--setting-sources=", "--settings",
        r#"{"disableAllHooks":true,"disableRemoteControl":true,"disableAgentView":true,"disableClaudeAiConnectors":true,"disableDeepLinkRegistration":"disable"}"#,
        "--tools", "", "--strict-mcp-config", "--mcp-config", r#"{"mcpServers":{}}"#,
        "--permission-mode", "dontAsk", "--no-chrome",
        "auth", "login",
    ]);
    let mut process = ClientProcess::spawn(command, MAX_OUTPUT)?;
    let result = process.wait_exit(Instant::now() + Duration::from_secs(180), cancel);
    process.finish();
    result
}

fn wait_response(
    process: &mut ClientProcess,
    id: u64,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> Result<Value, Error> {
    loop {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Acquire)) {
            return Err(Error::Cancelled);
        }
        let Some(message) = process.next_message(deadline)? else {
            continue;
        };
        if message.get("method").is_some() {
            continue;
        }
        return response_result(message, id);
    }
}

fn response_result(message: Value, expected_id: u64) -> Result<Value, Error> {
    if message.get("id").and_then(Value::as_u64) != Some(expected_id) {
        return Err(Error::Protocol);
    }
    if message.get("error").is_some() {
        return Err(Error::Failed);
    }
    message
        .get("result")
        .filter(|result| result.is_object())
        .cloned()
        .ok_or(Error::Protocol)
}

fn decode_message(bytes: &[u8], messages: usize) -> Result<Value, Error> {
    if messages > MAX_MESSAGES || bytes.len() > MAX_OUTPUT {
        return Err(Error::Protocol);
    }
    let message: Value = serde_json::from_slice(bytes).map_err(|_| Error::Protocol)?;
    if !message.is_object() {
        return Err(Error::Protocol);
    }
    if let Some(method) = message.get("method") {
        if method.as_str().is_none_or(str::is_empty)
            || message.get("id").is_some()
            || message.get("result").is_some()
            || message.get("error").is_some()
        {
            return Err(Error::Protocol);
        }
    } else if message.get("result").is_some() == message.get("error").is_some() {
        return Err(Error::Protocol);
    }
    Ok(message)
}

enum Output {
    Bytes(Vec<u8>),
    End,
    Invalid,
}

struct ClientProcess {
    child: Child,
    output: Option<Receiver<Output>>,
    reader: Option<JoinHandle<()>>,
    stop_reader: Arc<AtomicBool>,
    buffer: Vec<u8>,
    ended: bool,
    messages: usize,
    finished: bool,
}

impl ClientProcess {
    fn spawn(mut command: Command, limit: usize) -> Result<Self, Error> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let mut child = command.spawn().map_err(|_| Error::Failed)?;
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Failed);
        };
        // A descendant retaining stdout must not keep the reader alive after cleanup.
        let flags = unsafe { fcntl(stdout.as_raw_fd(), F_GETFL) };
        if flags < 0 || unsafe { fcntl(stdout.as_raw_fd(), F_SETFL, flags | O_NONBLOCK) } < 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Failed);
        }
        let (sender, output) = mpsc::sync_channel(8);
        let stop_reader = Arc::new(AtomicBool::new(false));
        let reader_cancel = Arc::clone(&stop_reader);
        let reader = thread::spawn(move || {
            let mut total = 0usize;
            loop {
                if reader_cancel.load(Ordering::Acquire) {
                    break;
                }
                let mut bytes = [0u8; 8192];
                match stdout.read(&mut bytes) {
                    Ok(0) => {
                        let _ = sender.send(Output::End);
                        break;
                    }
                    Ok(length) if total + length <= limit => {
                        total += length;
                        if sender
                            .send(Output::Bytes(bytes[..length].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    _ => {
                        let _ = sender.send(Output::Invalid);
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            output: Some(output),
            reader: Some(reader),
            stop_reader,
            buffer: Vec::new(),
            ended: false,
            messages: 0,
            finished: false,
        })
    }

    fn send(&mut self, value: &Value) -> Result<(), Error> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| Error::Protocol)?;
        bytes.push(b'\n');
        let stdin = self.child.stdin.as_mut().ok_or(Error::Failed)?;
        stdin
            .write_all(&bytes)
            .and_then(|()| stdin.flush())
            .map_err(|_| Error::Failed)
    }

    fn receive_output(&mut self, wait: Duration) -> Result<(), Error> {
        if self.ended {
            thread::sleep(wait);
            return Ok(());
        }
        match self
            .output
            .as_ref()
            .ok_or(Error::Protocol)?
            .recv_timeout(wait)
        {
            Ok(Output::Bytes(bytes)) => self.buffer.extend_from_slice(&bytes),
            Ok(Output::End) => self.ended = true,
            Ok(Output::Invalid) => return Err(Error::Protocol),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(Error::Protocol),
        }
        Ok(())
    }

    fn next_message(&mut self, deadline: Instant) -> Result<Option<Value>, Error> {
        if Instant::now() >= deadline {
            return Err(Error::TimedOut);
        }
        if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let bytes: Vec<u8> = self.buffer.drain(..=end).collect();
            self.messages += 1;
            let message = decode_message(&bytes, self.messages)?;
            return Ok(Some(message));
        }
        if self.ended {
            return Err(Error::Failed);
        }
        self.receive_output(POLL)?;
        Ok(None)
    }

    fn drain_to_eof(&mut self, deadline: Instant) -> Result<(), Error> {
        while !self.ended {
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
            self.receive_output(POLL)?;
        }
        Ok(())
    }

    fn wait_exit(&mut self, deadline: Instant, cancel: &AtomicBool) -> Result<(), Error> {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            self.receive_output(POLL)?;
            self.buffer.clear();
            if let Some(status) = self.child.try_wait().map_err(|_| Error::Failed)? {
                self.drain_to_eof(deadline)?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(Error::Failed)
                };
            }
            if Instant::now() >= deadline {
                return Err(Error::TimedOut);
            }
        }
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        drop(self.child.stdin.take());
        // EOF lets the official process finish an outstanding credential write first.
        if !self.wait_for_exit(Duration::from_secs(15)) {
            self.signal_group(SIGTERM);
            if !self.wait_for_exit(Duration::from_secs(15)) {
                self.signal_group(SIGKILL);
                let _ = self.child.wait();
            }
        }
        self.signal_group(SIGTERM);
        thread::sleep(Duration::from_millis(100));
        self.signal_group(SIGKILL);
        self.stop_reader.store(true, Ordering::Release);
        drop(self.output.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }

    fn wait_for_exit(&mut self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        loop {
            if self.receive_output(POLL).is_err() {
                thread::sleep(POLL);
            }
            self.buffer.clear();
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    fn signal_group(&self, signal: i32) {
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // Every child starts a new process group owned by this operation.
            unsafe {
                kill(-pid, signal);
            }
        }
    }
}

impl Drop for ClientProcess {
    fn drop(&mut self) {
        self.finish();
    }
}

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn fcntl(fd: i32, command: i32, ...) -> i32;
}

struct ScratchDirectory(PathBuf);

impl ScratchDirectory {
    fn create() -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Failed)?
            .as_nanos();
        for _ in 0..8 {
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "delta-v-auth-{}-{timestamp}-{number}",
                std::process::id()
            ));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(Error::Failed),
            }
        }
        Err(Error::Failed)
    }
}

impl Drop for ScratchDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_rpc_errors_never_reach_the_user() {
        let response = json!({"id": 2, "error": {"code": -32603, "message": "secret-token email@example.com", "data": {"refresh_token": "private"}}});
        let error = response_result(response, 2).expect_err("request must fail");
        assert_eq!(error, Error::Failed);
        assert!(!error.to_string().contains("secret-token"));
        assert!(!format!("{error:?}").contains("private"));
    }

    #[test]
    fn responses_must_match_the_outstanding_request() {
        assert_eq!(
            response_result(json!({"id": 4, "result": {}}), 2),
            Err(Error::Protocol)
        );
        assert_eq!(
            response_result(json!({"id": "2", "result": {}}), 2),
            Err(Error::Protocol)
        );
        assert_eq!(
            response_result(json!({"id": 2, "result": null}), 2),
            Err(Error::Protocol)
        );
    }

    #[test]
    fn authentication_urls_require_the_exact_official_https_host() {
        assert!(valid_auth_url(
            "https://auth.openai.com/oauth/authorize?state=opaque"
        ));
        assert!(valid_auth_url("https://chatgpt.com/auth/login"));
        for url in [
            "http://auth.openai.com/login",
            "https://auth.openai.com.evil.example/login",
            "https://auth.openai.com@evil.example/login",
            "https://user@auth.openai.com/login",
            "https://auth.openai.com:444/login",
            "file:///private/tmp/login",
            "https://chatgpt.com/login#script",
        ] {
            assert!(!valid_auth_url(url), "accepted {url}");
        }
    }

    #[test]
    fn versions_include_the_verified_desktop_prerelease() {
        assert!(supported_version(
            ProviderId::Codex,
            "codex-cli 0.154.0-alpha.6.2"
        ));
        assert!(supported_version(
            ProviderId::Claude,
            "2.1.270 (Claude Code)"
        ));
        assert!(!supported_version(ProviderId::Codex, "codex-cli 0.153.9"));
        assert!(!supported_version(
            ProviderId::Claude,
            "2.1.269 (Claude Code)"
        ));
        assert!(!supported_version(ProviderId::Codex, "unknown"));
    }

    #[test]
    fn server_requests_and_ambiguous_frames_are_rejected() {
        for value in [
            json!({"id": 7, "method": "account/chatgptAuthTokens/refresh", "params": {}}),
            json!({"id": 1, "result": {}, "error": {"message": "private"}}),
            json!({"method": 4}),
            json!({"method": "account/updated", "result": {}}),
        ] {
            assert_eq!(
                decode_message(&serde_json::to_vec(&value).expect("fixture"), 1),
                Err(Error::Protocol)
            );
        }
        assert!(decode_message(br#"{"method":"account/updated","params":{}}"#, 1).is_ok());
    }

    #[test]
    fn protocol_limits_apply_before_parsing() {
        assert_eq!(
            decode_message(br#"{"id":1,"result":{}}"#, MAX_MESSAGES + 1),
            Err(Error::Protocol)
        );
        assert_eq!(
            decode_message(&vec![b' '; MAX_OUTPUT + 1], 1),
            Err(Error::Protocol)
        );
    }

    #[test]
    fn relative_and_shell_expression_paths_are_not_executables() {
        assert!(executable_path(Path::new("codex")).is_none());
        assert!(executable_path(Path::new("$(touch /tmp/unwanted)")).is_none());
        assert!(executable_path(Path::new("/private/tmp")).is_none());
    }

    #[test]
    fn pre_cancelled_recovery_never_resolves_or_launches_a_client() {
        for action in [Action::Renew, Action::SignIn] {
            assert_eq!(
                recover(ProviderId::Codex, action, Arc::new(AtomicBool::new(true))),
                Err(Error::Cancelled)
            );
        }
    }

    #[test]
    fn eof_finishes_a_local_child_without_a_signal() {
        let mut command = Command::new("/bin/cat");
        command.env_clear();
        let mut process = ClientProcess::spawn(command, 1024).expect("local child");
        process
            .send(&json!({"id": 1, "result": {}}))
            .expect("write fixture");
        assert_eq!(
            wait_response(
                &mut process,
                1,
                Instant::now() + Duration::from_secs(2),
                None
            ),
            Ok(json!({})),
        );
        process.finish();
        assert!(
            process
                .child
                .try_wait()
                .expect("status")
                .expect("exited")
                .success()
        );
        assert!(process.reader.is_none());
        assert!(process.output.is_none());
    }

    #[test]
    fn child_output_over_the_limit_is_rejected_and_reaped() {
        let mut command = Command::new("/usr/bin/printf");
        command.env_clear().arg("0123456789");
        let mut process = ClientProcess::spawn(command, 4).expect("local child");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match process.receive_output(POLL) {
                Err(error) => {
                    assert_eq!(error, Error::Protocol);
                    break;
                }
                Ok(()) if Instant::now() < deadline => {}
                Ok(()) => panic!("oversized output was accepted"),
            }
        }
        process.finish();
        assert!(process.child.try_wait().expect("status").is_some());
        assert!(process.buffer.is_empty());
    }
}
