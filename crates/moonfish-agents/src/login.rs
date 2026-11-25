//! Interactive provider sign-in sessions across Local, WSL, and SSH locations.
//!
//! Each CLI runs over piped stdio via [`moonfish_transport::spawn`]:
//! - Antigravity: `agy acp --login`, parsing the Google OAuth URL from stdout and writing the
//!   pasted authorization code or callback URL to stdin.
//! - Claude Code: `claude auth login`, stripping OSC 8 and ANSI escapes to extract the sign-in
//!   URL, streaming stderr validation feedback, and accepting `code#state` on stdin (or
//!   localhost callback completion on Local/WSL).
//! - Codex: `codex app-server` JSON-RPC (`chatgpt` browser OAuth and `chatgptDeviceCode`), with
//!   automatic `-32602` fallback to `codex login --device-auth` and `-32600` fallback to browser
//!   redirect paste replayed via `curl -fsSL`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use moonfish_core::settings::{AgentKind, EnvVar, Location};
use moonfish_transport::{Proc, Spec, Stdin};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::account::{self, Account};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(25);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginMethod {
    #[default]
    Auto,
    Device,
    Browser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoginPromptKind {
    Code,
    Device,
    Browser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    pub session_id: String,
    pub agent: AgentKind,
    pub location: Location,
    pub kind: LoginPromptKind,
    pub url: String,
    pub user_code: Option<String>,
    pub auto_opened: bool,
    pub input_label: Option<String>,
    pub hint: Option<String>,
    pub can_switch_to_device: bool,
    pub can_switch_to_browser: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum LoginEvent {
    #[serde(rename_all = "camelCase")]
    Feedback { session_id: String, message: String },
    #[serde(rename_all = "camelCase")]
    Done { session_id: String, account: Account },
    #[serde(rename_all = "camelCase")]
    Error { session_id: String, message: String },
}

pub type LoginSink = Arc<dyn Fn(LoginEvent) + Send + Sync>;

type Watcher = Box<dyn FnOnce(Arc<ActiveSession>) + Send>;

#[derive(Default)]
pub struct LoginManager {
    sessions: Arc<Mutex<HashMap<String, Arc<ActiveSession>>>>,
}

enum SessionInput {
    Stdin { stdin: Mutex<Option<Stdin>> },
    CodexBrowser { stdin: Mutex<Option<Stdin>>, login_id: String, location: Location, env: Vec<EnvVar> },
    CodexDevice { stdin: Mutex<Option<Stdin>>, login_id: String },
    NoInput,
}

struct ActiveSession {
    agent: AgentKind,
    location: Location,
    proc: Mutex<Option<Proc>>,
    input: SessionInput,
    cancelled: AtomicBool,
    finished: AtomicBool,
}

impl ActiveSession {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        match &self.input {
            SessionInput::Stdin { stdin } => {
                stdin.lock().unwrap().take();
            }
            SessionInput::CodexBrowser { stdin, login_id, .. } | SessionInput::CodexDevice { stdin, login_id } => {
                if let Some(mut s) = stdin.lock().unwrap().take() {
                    let msg = json!({
                        "jsonrpc": "2.0",
                        "id": 99,
                        "method": "account/login/cancel",
                        "params": { "loginId": login_id }
                    });
                    let _ = writeln!(s, "{msg}");
                    let _ = s.flush();
                }
            }
            SessionInput::NoInput => {}
        }
        if let Some(mut proc) = self.proc.lock().unwrap().take() {
            moonfish_transport::stop_tree(&mut proc, Duration::from_millis(200));
        }
    }

    fn stop_proc(&self, grace: Duration) {
        match &self.input {
            SessionInput::Stdin { stdin } | SessionInput::CodexBrowser { stdin, .. } | SessionInput::CodexDevice { stdin, .. } => {
                stdin.lock().unwrap().take();
            }
            SessionInput::NoInput => {}
        }
        if let Some(mut proc) = self.proc.lock().unwrap().take() {
            moonfish_transport::stop_tree(&mut proc, grace);
        }
    }
}

impl LoginManager {
    pub fn start(
        &self,
        agent: AgentKind,
        binary: &str,
        location: Location,
        env: Vec<EnvVar>,
        method: LoginMethod,
        sink: LoginSink,
    ) -> Result<LoginStart, String> {
        self.cancel_for(agent, &location);
        let session_id = next_session_id();
        let (start, session, watcher) = match agent {
            AgentKind::Antigravity => start_antigravity(&session_id, binary, &location, &env, sink)?,
            AgentKind::Claude => start_claude(&session_id, binary, &location, &env, sink)?,
            AgentKind::Codex => start_codex(&session_id, binary, &location, &env, method, sink)?,
        };
        self.sessions.lock().unwrap().insert(session_id.clone(), session.clone());
        let sessions = self.sessions.clone();
        let sid = session_id;
        thread::spawn(move || {
            watcher(session);
            sessions.lock().unwrap().remove(&sid);
        });
        Ok(start)
    }

    pub fn submit(&self, session_id: &str, input: &str) -> Result<(), String> {
        let session = self.sessions.lock().unwrap().get(session_id).cloned();
        let Some(session) = session else { return Err("That sign-in session is no longer active.".into()) };
        if session.cancelled.load(Ordering::SeqCst) || session.finished.load(Ordering::SeqCst) {
            return Err("That sign-in session has already ended.".into());
        }
        match &session.input {
            SessionInput::Stdin { stdin } => {
                let trimmed = validate_stdin_code(input)?;
                let mut guard = stdin.lock().unwrap();
                let w = guard.as_mut().ok_or_else(|| "Sign-in input is closed.".to_string())?;
                writeln!(w, "{trimmed}").and_then(|_| w.flush()).map_err(|e| format!("Could not send sign-in code: {e}"))
            }
            SessionInput::CodexBrowser { location, env, .. } => {
                let url = validate_codex_callback_url(input)?;
                let cwd = home_cwd(location);
                let args = vec!["-fsSL".into(), "--".into(), url];
                let spec = Spec { location, cwd: &cwd, program: "curl", args, env };
                let out = moonfish_transport::run(&spec, None, Duration::from_secs(20));
                if session.finished.load(Ordering::SeqCst) {
                    return Ok(());
                }
                match out {
                    Ok(o) if o.ok() => Ok(()),
                    Ok(o) => {
                        let fallback = "Could not reach the local callback listener.";
                        let detail = last_non_empty(&o.stderr).or_else(|| last_non_empty(&o.stdout)).unwrap_or(fallback);
                        Err(format!("Callback replay failed: {detail}"))
                    }
                    Err(e) => Err(format!("Callback replay failed: {e}")),
                }
            }
            SessionInput::CodexDevice { .. } | SessionInput::NoInput => Err("This sign-in flow does not accept pasted input.".into()),
        }
    }

    pub fn cancel(&self, session_id: &str) -> Result<(), String> {
        if let Some(session) = self.sessions.lock().unwrap().remove(session_id) {
            session.cancel();
        }
        Ok(())
    }

    pub fn cancel_for(&self, agent: AgentKind, location: &Location) {
        let to_cancel: Vec<Arc<ActiveSession>> = {
            let mut map = self.sessions.lock().unwrap();
            let ids: Vec<String> =
                map.iter().filter(|(_, s)| s.agent == agent && s.location == *location).map(|(id, _)| id.clone()).collect();
            ids.into_iter().filter_map(|id| map.remove(&id)).collect()
        };
        for session in to_cancel {
            session.cancel();
        }
    }

    pub fn cancel_all(&self) {
        let all: Vec<Arc<ActiveSession>> = self.sessions.lock().unwrap().drain().map(|(_, s)| s).collect();
        for session in all {
            session.cancel();
        }
    }
}

impl Drop for LoginManager {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

fn next_session_id() -> String {
    let seq = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    format!("login-{ts:x}-{seq}")
}

fn program(kind: AgentKind, binary: &str) -> String {
    if binary.trim().is_empty() {
        kind.default_binary().into()
    } else {
        binary.trim().into()
    }
}

fn home_cwd(location: &Location) -> String {
    if matches!(location, Location::Local) {
        moonfish_core::paths::home_dir().to_string_lossy().into_owned()
    } else {
        "~".into()
    }
}

fn env_var(key: &str, value: &str) -> EnvVar {
    EnvVar { key: key.into(), value: value.into(), in_keychain: false }
}

fn last_non_empty(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

fn validate_stdin_code(raw: &str) -> Result<&str, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Enter the authorization code before submitting.".into());
    }
    if trimmed.chars().any(|c| matches!(c, '\r' | '\n' | '\0')) {
        return Err("Paste a single authorization code or callback URL.".into());
    }
    Ok(trimmed)
}

fn spawn_line_reader(out: moonfish_transport::Out) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn spawn_stderr_collector(err: moonfish_transport::Out) -> (Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let dest = lines.clone();
    let handle = thread::spawn(move || {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            let cleaned = strip_ansi_and_osc(&line);
            let trimmed = cleaned.trim();
            if !trimmed.is_empty() {
                dest.lock().unwrap().push(trimmed.to_string());
            }
        }
    });
    (lines, handle)
}

// ---- Parsers & validators --------------------------------------------------------------------

/// Strips CSI ANSI escapes (`\x1b[...m`), OSC sequences (`\x1b]...\x07` or `\x1b]...\x1b\\`),
/// and carriage returns.
pub fn strip_ansi_and_osc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            continue;
        }
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ('@'..='~').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn extract_https_url(text: &str, required_prefix: &str) -> Option<String> {
    let cleaned = strip_ansi_and_osc(text);
    let start = cleaned.find(required_prefix)?;
    let tail = &cleaned[start..];
    let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']')).unwrap_or(tail.len());
    let url = tail[..end].trim_end_matches(['.', ',', ';']);
    (url.len() > required_prefix.len()).then(|| url.to_string())
}

/// Extracts the Google OAuth URL printed by `agy acp --login`.
pub fn extract_antigravity_url(text: &str) -> Option<String> {
    extract_https_url(text, "https://accounts.google.com/o/oauth2/auth?")
}

/// Interprets `agy acp --login` stdout/stderr after exit.
pub fn parse_antigravity_result(stdout: &str, stderr: &str, exit_ok: bool) -> Result<Option<String>, String> {
    let cleaned_out = strip_ansi_and_osc(stdout);
    let cleaned_err = strip_ansi_and_osc(stderr);
    for line in cleaned_out.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Logged in as ") {
            let email = rest.trim().trim_end_matches('.').trim();
            return Ok((!email.is_empty()).then(|| email.to_string()));
        }
    }
    if exit_ok {
        return Ok(None);
    }
    for line in cleaned_out.lines().chain(cleaned_err.lines()).map(str::trim) {
        let lower = line.to_lowercase();
        if lower.starts_with("couldn't log in:") || lower.starts_with("login cancelled:") || lower.starts_with("error:") {
            return Err(line.to_string());
        }
    }
    Err(last_non_empty(&cleaned_err).or_else(|| last_non_empty(&cleaned_out)).unwrap_or("Antigravity sign-in failed.").to_string())
}

/// Extracts the sign-in URL from `claude auth login` stdout (stripping OSC 8 hyperlinks and ANSI).
pub fn extract_claude_url(text: &str) -> Option<String> {
    extract_https_url(text, "https://")
}

/// Whether a cleaned `claude auth login` stdout line signals completion.
pub fn is_claude_success_line(line: &str) -> bool {
    strip_ansi_and_osc(line).to_lowercase().contains("login successful")
}

/// Whether a cleaned `claude auth login` stderr line is non-fatal validation feedback.
pub fn is_claude_feedback_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();
    !lower.starts_with("couldn't log in")
}

/// Extracts `(verification_url, user_code)` from `codex login --device-auth` stdout.
pub fn parse_codex_device_auth(text: &str) -> (Option<String>, Option<String>) {
    let cleaned = strip_ansi_and_osc(text);
    let url = extract_https_url(&cleaned, "https://");
    let mut after_prompt = false;
    let mut code = None;
    for raw in cleaned.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_lowercase();
        if lower.contains("one-time code") || lower.contains("enter this") || line.starts_with("https://") {
            after_prompt = true;
            continue;
        }
        if after_prompt && is_device_user_code(line) {
            code = Some(line.to_string());
            break;
        }
    }
    (url, code)
}

fn is_device_user_code(s: &str) -> bool {
    let len = s.len();
    if !(4..=32).contains(&len) || s.starts_with("http") {
        return false;
    }
    s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && s.bytes().any(|b| b.is_ascii_alphanumeric())
}

pub fn parse_codex_device_cli_error(stdout: &str, stderr: &str) -> String {
    let cleaned_err = strip_ansi_and_osc(stderr);
    let cleaned_out = strip_ansi_and_osc(stdout);
    for line in cleaned_err.lines().chain(cleaned_out.lines()).map(str::trim) {
        if let Some(rest) = line.strip_prefix("Error logging in:") {
            let msg = rest.trim();
            if !msg.is_empty() {
                return msg.to_string();
            }
        }
    }
    last_non_empty(&cleaned_err).or_else(|| last_non_empty(&cleaned_out)).unwrap_or("Codex device sign-in failed.").to_string()
}

/// Parses a Codex `account/login/completed` notification matching `expected_login_id`.
pub fn parse_codex_login_completed(v: &Value, expected_login_id: &str) -> Option<Result<(), String>> {
    if v["method"].as_str() != Some("account/login/completed") {
        return None;
    }
    let params = &v["params"];
    if params["loginId"].as_str().is_some_and(|id| id != expected_login_id) {
        return None;
    }
    if params["success"].as_bool() == Some(true) {
        Some(Ok(()))
    } else {
        let msg = params["error"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("Codex sign-in failed.");
        Some(Err(msg.to_string()))
    }
}

/// Validates a Codex localhost OAuth callback URL before replaying it with `curl -fsSL`.
///
/// Only `http://(127.0.0.1|localhost):(1455|1457)/auth/callback?...` with safe URL characters
/// and a non-empty `code` query parameter is accepted.
pub fn validate_codex_callback_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("Paste the http://127.0.0.1 callback URL from your browser.".into());
    }
    if url.len() > 4096 {
        return Err("Callback URL is too long.".into());
    }
    const PREFIXES: [&str; 4] = [
        "http://127.0.0.1:1455/auth/callback?",
        "http://127.0.0.1:1457/auth/callback?",
        "http://localhost:1455/auth/callback?",
        "http://localhost:1457/auth/callback?",
    ];
    let Some(query) = PREFIXES.iter().find_map(|p| url.strip_prefix(p)) else {
        return Err("Expected a callback URL starting with http://127.0.0.1:1455/auth/callback? (or localhost / port 1457).".into());
    };
    if query.is_empty() {
        return Err("Callback URL is missing query parameters.".into());
    }
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() || !bytes[i + 1].is_ascii_hexdigit() || !bytes[i + 2].is_ascii_hexdigit() {
                return Err("Callback URL contains an invalid percent-encoded sequence.".into());
            }
            i += 3;
            continue;
        }
        let allowed = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'=' | b'&' | b'+' | b':' | b'/' | b',');
        if !allowed {
            return Err("Callback URL contains unsupported characters.".into());
        }
        i += 1;
    }
    let has_code = query.split('&').any(|pair| pair.strip_prefix("code=").is_some_and(|v| !v.is_empty()));
    if !has_code {
        return Err("Callback URL is missing the code parameter.".into());
    }
    Ok(url.to_string())
}

// ---- Antigravity -----------------------------------------------------------------------------

fn start_antigravity(
    session_id: &str,
    binary: &str,
    location: &Location,
    env: &[EnvVar],
    sink: LoginSink,
) -> Result<(LoginStart, Arc<ActiveSession>, Watcher), String> {
    let prog = program(AgentKind::Antigravity, binary);
    let cwd = home_cwd(location);
    let mut run_env = env.to_vec();
    crate::antigravity::acp_env_for(location, &mut run_env);
    if matches!(location, Location::Ssh { .. }) && !run_env.iter().any(|e| e.key == "SSH_CONNECTION") {
        run_env.push(env_var("SSH_CONNECTION", "1"));
    }
    let args = vec!["acp".into(), "--login".into()];
    let spec = Spec { location, cwd: &cwd, program: &prog, args, env: &run_env };
    let mut proc = moonfish_transport::spawn(&spec).map_err(|e| format!("Could not start {prog}: {e}"))?;
    let stdin = proc.stdin.take().ok_or("no stdin")?;
    let stdout = proc.stdout.take().ok_or("no stdout")?;
    let stderr = proc.stderr.take().ok_or("no stderr")?;
    let stdout_rx = spawn_line_reader(stdout);
    let (stderr_lines, stderr_handle) = spawn_stderr_collector(stderr);

    let mut seen_stdout = Vec::new();
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let url = loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
            return Err("Timed out waiting for Antigravity sign-in link.".into());
        };
        match stdout_rx.recv_timeout(remaining) {
            Ok(line) => {
                if let Some(found) = extract_antigravity_url(&line) {
                    seen_stdout.push(line);
                    break found;
                }
                seen_stdout.push(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                return Err("Timed out waiting for Antigravity sign-in link.".into());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let code = proc.wait().ok().and_then(|s| s.code());
                let _ = stderr_handle.join();
                if code == Some(127) {
                    return Err(format!("{prog} is not installed there."));
                }
                let stderr_text = stderr_lines.lock().unwrap().join("\n");
                let parsed = parse_antigravity_result(&seen_stdout.join("\n"), &stderr_text, false);
                let fallback = "Antigravity exited without printing a sign-in link.".to_string();
                return Err(parsed.err().unwrap_or(fallback));
            }
        }
    };

    let session = Arc::new(ActiveSession {
        agent: AgentKind::Antigravity,
        location: location.clone(),
        proc: Mutex::new(Some(proc)),
        input: SessionInput::Stdin { stdin: Mutex::new(Some(stdin)) },
        cancelled: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let start = LoginStart {
        session_id: session_id.to_string(),
        agent: AgentKind::Antigravity,
        location: location.clone(),
        kind: LoginPromptKind::Code,
        url,
        user_code: None,
        auto_opened: matches!(location, Location::Local),
        input_label: Some("Authorization code or callback URL".into()),
        hint: Some("Sign in with Google in your browser, then paste the authorization code or callback URL.".into()),
        can_switch_to_device: false,
        can_switch_to_browser: false,
    };

    let sid = session_id.to_string();
    let binary = binary.to_string();
    let location = location.clone();
    let env = env.to_vec();
    let watcher: Watcher = Box::new(move |session| {
        for line in stdout_rx {
            seen_stdout.push(line);
        }
        let _ = stderr_handle.join();
        let exit_ok = session.proc.lock().unwrap().take().and_then(|mut p| p.wait().ok()).is_some_and(|s| s.success());
        if session.cancelled.load(Ordering::SeqCst) || session.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let stderr_text = stderr_lines.lock().unwrap().join("\n");
        match parse_antigravity_result(&seen_stdout.join("\n"), &stderr_text, exit_ok) {
            Ok(email_hint) => {
                let mut acct = account::account(AgentKind::Antigravity, &binary, &location, &env);
                if !acct.signed_in {
                    acct.signed_in = true;
                    acct.error = None;
                    if acct.label.is_none() {
                        acct.label = email_hint;
                    }
                }
                sink(LoginEvent::Done { session_id: sid, account: acct });
            }
            Err(message) => sink(LoginEvent::Error { session_id: sid, message }),
        }
    });

    Ok((start, session, watcher))
}

// ---- Claude Code -----------------------------------------------------------------------------

fn start_claude(
    session_id: &str,
    binary: &str,
    location: &Location,
    env: &[EnvVar],
    sink: LoginSink,
) -> Result<(LoginStart, Arc<ActiveSession>, Watcher), String> {
    let prog = program(AgentKind::Claude, binary);
    let cwd = home_cwd(location);
    let mut run_env = env.to_vec();
    if matches!(location, Location::Ssh { .. }) && !run_env.iter().any(|e| e.key == "BROWSER") {
        run_env.push(env_var("BROWSER", "moonfish-no-browser"));
    }
    let args = vec!["auth".into(), "login".into()];
    let spec = Spec { location, cwd: &cwd, program: &prog, args, env: &run_env };
    let mut proc = moonfish_transport::spawn(&spec).map_err(|e| format!("Could not start {prog}: {e}"))?;
    let stdin = proc.stdin.take().ok_or("no stdin")?;
    let stdout = proc.stdout.take().ok_or("no stdout")?;
    let stderr = proc.stderr.take().ok_or("no stderr")?;
    let stdout_rx = spawn_line_reader(stdout);

    let session = Arc::new(ActiveSession {
        agent: AgentKind::Claude,
        location: location.clone(),
        proc: Mutex::new(Some(proc)),
        input: SessionInput::Stdin { stdin: Mutex::new(Some(stdin)) },
        cancelled: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });

    let ready = Arc::new(AtomicBool::new(false));
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_handle = {
        let lines = stderr_lines.clone();
        let ready = ready.clone();
        let session = session.clone();
        let sink = sink.clone();
        let sid = session_id.to_string();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let cleaned = strip_ansi_and_osc(&line);
                let trimmed = cleaned.trim();
                if trimmed.is_empty() {
                    continue;
                }
                lines.lock().unwrap().push(trimmed.to_string());
                if ready.load(Ordering::SeqCst)
                    && !session.cancelled.load(Ordering::SeqCst)
                    && !session.finished.load(Ordering::SeqCst)
                    && is_claude_feedback_line(trimmed)
                {
                    sink(LoginEvent::Feedback { session_id: sid.clone(), message: trimmed.to_string() });
                }
            }
        })
    };

    let mut seen_stdout = Vec::new();
    let mut saw_success = false;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let url = loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            session.cancel();
            return Err("Timed out waiting for Claude Code sign-in link.".into());
        };
        match stdout_rx.recv_timeout(remaining) {
            Ok(line) => {
                if is_claude_success_line(&line) {
                    saw_success = true;
                }
                if let Some(found) = extract_claude_url(&line) {
                    seen_stdout.push(line);
                    break found;
                }
                seen_stdout.push(line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                session.cancel();
                return Err("Timed out waiting for Claude Code sign-in link.".into());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let code = session.proc.lock().unwrap().take().and_then(|mut p| p.wait().ok()).and_then(|s| s.code());
                let _ = stderr_handle.join();
                if code == Some(127) {
                    return Err(format!("{prog} is not installed there."));
                }
                let stderr_text = stderr_lines.lock().unwrap().join("\n");
                let out_text = strip_ansi_and_osc(&seen_stdout.join("\n"));
                let fallback = "Claude Code exited without printing a sign-in link.";
                let msg = last_non_empty(&stderr_text).or_else(|| last_non_empty(&out_text)).unwrap_or(fallback);
                return Err(msg.to_string());
            }
        }
    };

    ready.store(true, Ordering::SeqCst);
    let hint = if matches!(location, Location::Ssh { .. }) {
        "Open the sign-in page, copy the full code including #, and paste it below."
    } else {
        "Finish signing in in your browser, or paste the code#state value below."
    };
    let start = LoginStart {
        session_id: session_id.to_string(),
        agent: AgentKind::Claude,
        location: location.clone(),
        kind: LoginPromptKind::Code,
        url,
        user_code: None,
        auto_opened: !matches!(location, Location::Ssh { .. }),
        input_label: Some("Authorization code (code#state)".into()),
        hint: Some(hint.into()),
        can_switch_to_device: false,
        can_switch_to_browser: false,
    };

    let sid = session_id.to_string();
    let binary = binary.to_string();
    let location = location.clone();
    let env = env.to_vec();
    let watcher: Watcher = Box::new(move |session| {
        for line in stdout_rx {
            if is_claude_success_line(&line) {
                saw_success = true;
            }
            seen_stdout.push(line);
        }
        let _ = stderr_handle.join();
        let exit_ok = session.proc.lock().unwrap().take().and_then(|mut p| p.wait().ok()).is_some_and(|s| s.success());
        if session.cancelled.load(Ordering::SeqCst) || session.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        if exit_ok || saw_success {
            let acct = account::account(AgentKind::Claude, &binary, &location, &env);
            if acct.signed_in {
                sink(LoginEvent::Done { session_id: sid, account: acct });
            } else {
                let locked = is_macos_ssh_keychain_locked(&location, &env);
                sink(LoginEvent::Error { session_id: sid, message: claude_post_login_error(&acct, locked) });
            }
        } else {
            let stderr_text = stderr_lines.lock().unwrap().join("\n");
            let out_text = strip_ansi_and_osc(&seen_stdout.join("\n"));
            let msg = last_non_empty(&stderr_text).or_else(|| last_non_empty(&out_text)).unwrap_or("Claude Code sign-in failed.");
            sink(LoginEvent::Error { session_id: sid, message: msg.to_string() });
        }
    });

    Ok((start, session, watcher))
}

fn is_macos_ssh_keychain_locked(location: &Location, env: &[EnvVar]) -> bool {
    if !matches!(location, Location::Ssh { .. }) {
        return false;
    }
    let script = concat!(
        "command -v security >/dev/null 2>&1 ",
        r#"&& ! security show-keychain-info "$HOME/Library/Keychains/login.keychain-db" >/dev/null 2>&1"#
    );
    let spec = Spec { location, cwd: "~", program: "sh", args: vec!["-c".into(), script.into()], env };
    moonfish_transport::run(&spec, None, Duration::from_secs(10)).is_ok_and(|o| o.ok())
}

fn claude_post_login_error(acct: &Account, keychain_locked: bool) -> String {
    if keychain_locked {
        concat!(
            "Claude Code finished signing in, but the macOS login keychain on this SSH host is locked. ",
            "Unlock it with `security unlock-keychain ~/Library/Keychains/login.keychain-db` ",
            "or set `CLAUDE_CODE_OAUTH_TOKEN` in Settings under Providers."
        )
        .into()
    } else if let Some(err) = acct.error.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        format!("Claude Code finished signing in, but `claude auth status` still reports it is not signed in: {err}")
    } else {
        concat!(
            "Claude Code finished signing in, but `claude auth status` still reports it is not signed in. ",
            "Check credential storage or set `CLAUDE_CODE_OAUTH_TOKEN` in Settings under Providers."
        )
        .into()
    }
}

// ---- Codex -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexMode {
    Browser,
    Device,
}

fn resolve_codex_method(location: &Location, method: LoginMethod) -> CodexMode {
    match (method, location) {
        (LoginMethod::Device, _) | (LoginMethod::Auto, Location::Ssh { .. }) => CodexMode::Device,
        (LoginMethod::Browser, _) | (LoginMethod::Auto, _) => CodexMode::Browser,
    }
}

struct RpcError {
    code: Option<i64>,
    message: String,
}

fn parse_rpc_error(err: &Value) -> RpcError {
    let code = err["code"].as_i64();
    let message = crate::jsonrpc::error_text(err, "Codex");
    RpcError { code, message }
}

fn rpc_send(stdin: &mut Stdin, v: &Value) -> Result<(), String> {
    writeln!(stdin, "{v}").and_then(|_| stdin.flush()).map_err(|e| format!("Could not write to Codex: {e}"))
}

enum RpcReply {
    Ok(Value),
    RpcErr(RpcError),
}

fn rpc_wait(rx: &mpsc::Receiver<Value>, expected_id: u64, timeout: Duration) -> Result<RpcReply, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else { return Err("Codex did not respond in time.".into()) };
        match rx.recv_timeout(remaining) {
            Ok(v) => {
                if v.get("method").is_none() && v["id"].as_u64() == Some(expected_id) {
                    return match v.get("error") {
                        Some(err) => Ok(RpcReply::RpcErr(parse_rpc_error(err))),
                        None => Ok(RpcReply::Ok(v["result"].clone())),
                    };
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return Err("Codex did not respond in time.".into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Codex exited during sign-in setup.".into()),
        }
    }
}

fn start_codex(
    session_id: &str,
    binary: &str,
    location: &Location,
    env: &[EnvVar],
    method: LoginMethod,
    sink: LoginSink,
) -> Result<(LoginStart, Arc<ActiveSession>, Watcher), String> {
    let prog = program(AgentKind::Codex, binary);
    let cwd = home_cwd(location);
    let spec = Spec { location, cwd: &cwd, program: &prog, args: vec!["app-server".into()], env };
    let mut proc = moonfish_transport::spawn(&spec).map_err(|e| format!("Could not start {prog}: {e}"))?;
    let mut stdin = proc.stdin.take().ok_or("no stdin")?;
    let stdout = proc.stdout.take().ok_or("no stdout")?;
    let stderr = proc.stderr.take().ok_or("no stderr")?;

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    let (stderr_lines, stderr_handle) = spawn_stderr_collector(stderr);

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "moonfish", "title": "Moonfish", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": null
        }
    });
    let init_res = rpc_send(&mut stdin, &init).and_then(|_| rpc_wait(&rx, 1, STARTUP_TIMEOUT)).and_then(|reply| match reply {
        RpcReply::Ok(_) => Ok(()),
        RpcReply::RpcErr(e) => Err(e.message),
    });
    if let Err(e) = init_res {
        drop(stdin);
        moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
        let _ = stderr_handle.join();
        let detail = stderr_lines.lock().unwrap().last().cloned();
        return Err(detail.unwrap_or(e));
    }
    if let Err(e) = rpc_send(&mut stdin, &json!({"jsonrpc": "2.0", "method": "initialized"})) {
        drop(stdin);
        moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
        return Err(e);
    }

    let mode = resolve_codex_method(location, method);
    let (resolved_mode, res, device_disabled) = match mode {
        CodexMode::Browser => {
            let req = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "account/login/start",
                "params": { "type": "chatgpt", "useHostedLoginSuccessPage": true }
            });
            match rpc_send(&mut stdin, &req).and_then(|_| rpc_wait(&rx, 2, STARTUP_TIMEOUT)) {
                Ok(RpcReply::Ok(v)) => (CodexMode::Browser, v, false),
                Ok(RpcReply::RpcErr(e)) => {
                    drop(stdin);
                    moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                    return Err(e.message);
                }
                Err(e) => {
                    drop(stdin);
                    moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                    return Err(e);
                }
            }
        }
        CodexMode::Device => {
            let req = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "account/login/start",
                "params": { "type": "chatgptDeviceCode" }
            });
            match rpc_send(&mut stdin, &req).and_then(|_| rpc_wait(&rx, 2, STARTUP_TIMEOUT)) {
                Ok(RpcReply::Ok(v)) => (CodexMode::Device, v, false),
                Ok(RpcReply::RpcErr(e)) if e.code == Some(-32602) => {
                    drop(stdin);
                    moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                    return start_codex_device_cli(session_id, binary, location, env, sink);
                }
                Ok(RpcReply::RpcErr(e)) if e.code == Some(-32600) => {
                    let fallback = json!({
                        "jsonrpc": "2.0",
                        "id": 3,
                        "method": "account/login/start",
                        "params": { "type": "chatgpt", "useHostedLoginSuccessPage": true }
                    });
                    match rpc_send(&mut stdin, &fallback).and_then(|_| rpc_wait(&rx, 3, STARTUP_TIMEOUT)) {
                        Ok(RpcReply::Ok(v)) => (CodexMode::Browser, v, true),
                        Ok(RpcReply::RpcErr(err)) => {
                            drop(stdin);
                            moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                            return Err(err.message);
                        }
                        Err(err) => {
                            drop(stdin);
                            moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                            return Err(err);
                        }
                    }
                }
                Ok(RpcReply::RpcErr(e)) => {
                    drop(stdin);
                    moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                    return Err(e.message);
                }
                Err(e) => {
                    drop(stdin);
                    moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                    return Err(e);
                }
            }
        }
    };

    let Some(login_id) = res["loginId"].as_str().filter(|s| !s.is_empty()).map(String::from) else {
        drop(stdin);
        moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
        return Err("Codex did not return a login session id.".into());
    };

    let (start, input) = match resolved_mode {
        CodexMode::Browser => {
            let Some(auth_url) = res["authUrl"].as_str().filter(|s| !s.is_empty()).map(String::from) else {
                drop(stdin);
                moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                return Err("Codex did not return a sign-in URL.".into());
            };
            let is_ssh = matches!(location, Location::Ssh { .. });
            let input_label = if is_ssh {
                "Paste the http://127.0.0.1 callback URL from your browser"
            } else {
                "Callback URL (optional if browser didn't redirect)"
            };
            let hint = if device_disabled {
                "Device code login is disabled for this account. \
                 Open the sign-in page, then paste the http://127.0.0.1 callback URL."
            } else if is_ssh {
                "Open the sign-in page in your browser. \
                 When the localhost redirect fails to load, copy that full URL and paste it below."
            } else {
                "Open the sign-in page in your browser to finish automatically, \
                 or paste the callback URL below if it did not redirect."
            };
            let start = LoginStart {
                session_id: session_id.to_string(),
                agent: AgentKind::Codex,
                location: location.clone(),
                kind: LoginPromptKind::Browser,
                url: auth_url,
                user_code: None,
                auto_opened: false,
                input_label: Some(input_label.into()),
                hint: Some(hint.into()),
                can_switch_to_device: true,
                can_switch_to_browser: false,
            };
            let input = SessionInput::CodexBrowser {
                stdin: Mutex::new(Some(stdin)),
                login_id: login_id.clone(),
                location: location.clone(),
                env: env.to_vec(),
            };
            (start, input)
        }
        CodexMode::Device => {
            let Some(verification_url) = res["verificationUrl"].as_str().filter(|s| !s.is_empty()).map(String::from) else {
                drop(stdin);
                moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                return Err("Codex did not return a device verification URL.".into());
            };
            let Some(user_code) = res["userCode"].as_str().filter(|s| !s.is_empty()).map(String::from) else {
                drop(stdin);
                moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                return Err("Codex did not return a device code.".into());
            };
            let hint = "Open the sign-in page and enter this code. \
                        Device code sign-in must be enabled in ChatGPT Security settings.";
            let start = LoginStart {
                session_id: session_id.to_string(),
                agent: AgentKind::Codex,
                location: location.clone(),
                kind: LoginPromptKind::Device,
                url: verification_url,
                user_code: Some(user_code),
                auto_opened: false,
                input_label: None,
                hint: Some(hint.into()),
                can_switch_to_device: false,
                can_switch_to_browser: true,
            };
            let input = SessionInput::CodexDevice { stdin: Mutex::new(Some(stdin)), login_id: login_id.clone() };
            (start, input)
        }
    };

    let session = Arc::new(ActiveSession {
        agent: AgentKind::Codex,
        location: location.clone(),
        proc: Mutex::new(Some(proc)),
        input,
        cancelled: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });

    let sid = session_id.to_string();
    let binary = binary.to_string();
    let location = location.clone();
    let env = env.to_vec();
    let watcher: Watcher = Box::new(move |session| {
        for v in rx {
            let Some(outcome) = parse_codex_login_completed(&v, &login_id) else { continue };
            session.stop_proc(Duration::from_millis(200));
            if session.cancelled.load(Ordering::SeqCst) || session.finished.swap(true, Ordering::SeqCst) {
                return;
            }
            match outcome {
                Ok(()) => {
                    let mut acct = account::account(AgentKind::Codex, &binary, &location, &env);
                    if !acct.signed_in {
                        acct.signed_in = true;
                        acct.error = None;
                    }
                    sink(LoginEvent::Done { session_id: sid, account: acct });
                }
                Err(message) => sink(LoginEvent::Error { session_id: sid, message }),
            }
            return;
        }
        let _ = stderr_handle.join();
        session.stop_proc(Duration::ZERO);
        if session.cancelled.load(Ordering::SeqCst) || session.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let msg = stderr_lines.lock().unwrap().last().cloned().unwrap_or_else(|| "Codex sign-in ended before completing.".into());
        sink(LoginEvent::Error { session_id: sid, message: msg });
    });

    Ok((start, session, watcher))
}

fn start_codex_device_cli(
    session_id: &str,
    binary: &str,
    location: &Location,
    env: &[EnvVar],
    sink: LoginSink,
) -> Result<(LoginStart, Arc<ActiveSession>, Watcher), String> {
    let prog = program(AgentKind::Codex, binary);
    let cwd = home_cwd(location);
    let args = vec!["login".into(), "--device-auth".into()];
    let spec = Spec { location, cwd: &cwd, program: &prog, args, env };
    let mut proc = moonfish_transport::spawn(&spec).map_err(|e| format!("Could not start {prog}: {e}"))?;
    let _ = proc.stdin.take();
    let stdout = proc.stdout.take().ok_or("no stdout")?;
    let stderr = proc.stderr.take().ok_or("no stderr")?;
    let stdout_rx = spawn_line_reader(stdout);
    let (stderr_lines, stderr_handle) = spawn_stderr_collector(stderr);

    let mut seen_stdout = Vec::new();
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let (url, user_code) = loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
            return Err("Timed out waiting for Codex device sign-in code.".into());
        };
        match stdout_rx.recv_timeout(remaining) {
            Ok(line) => {
                seen_stdout.push(line);
                let joined = seen_stdout.join("\n");
                if let (Some(u), Some(c)) = parse_codex_device_auth(&joined) {
                    break (u, c);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                moonfish_transport::stop_tree(&mut proc, Duration::ZERO);
                return Err("Timed out waiting for Codex device sign-in code.".into());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = proc.wait();
                let _ = stderr_handle.join();
                let stderr_text = stderr_lines.lock().unwrap().join("\n");
                return Err(parse_codex_device_cli_error(&seen_stdout.join("\n"), &stderr_text));
            }
        }
    };

    let session = Arc::new(ActiveSession {
        agent: AgentKind::Codex,
        location: location.clone(),
        proc: Mutex::new(Some(proc)),
        input: SessionInput::NoInput,
        cancelled: AtomicBool::new(false),
        finished: AtomicBool::new(false),
    });
    let start = LoginStart {
        session_id: session_id.to_string(),
        agent: AgentKind::Codex,
        location: location.clone(),
        kind: LoginPromptKind::Device,
        url,
        user_code: Some(user_code),
        auto_opened: false,
        input_label: None,
        hint: Some("Open the sign-in page and enter this one-time code.".into()),
        can_switch_to_device: false,
        can_switch_to_browser: true,
    };

    let sid = session_id.to_string();
    let binary = binary.to_string();
    let location = location.clone();
    let env = env.to_vec();
    let watcher: Watcher = Box::new(move |session| {
        for line in stdout_rx {
            seen_stdout.push(line);
        }
        let _ = stderr_handle.join();
        let exit_ok = session.proc.lock().unwrap().take().and_then(|mut p| p.wait().ok()).is_some_and(|s| s.success());
        if session.cancelled.load(Ordering::SeqCst) || session.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        if exit_ok {
            let mut acct = account::account(AgentKind::Codex, &binary, &location, &env);
            if !acct.signed_in {
                acct.signed_in = true;
                acct.error = None;
            }
            sink(LoginEvent::Done { session_id: sid, account: acct });
        } else {
            let stderr_text = stderr_lines.lock().unwrap().join("\n");
            let message = parse_codex_device_cli_error(&seen_stdout.join("\n"), &stderr_text);
            sink(LoginEvent::Error { session_id: sid, message });
        }
    });

    Ok((start, session, watcher))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_antigravity_stdout_url_and_results() {
        let out = "Open this link in a browser to log in with Google:\n\n  \
            https://accounts.google.com/o/oauth2/auth?access_type=offline&client_id=abc&state=xyz\n\n\
            Then paste the authorization code, or the full URL of the page you land on, and press Enter:\n";
        assert_eq!(
            extract_antigravity_url(out).as_deref(),
            Some("https://accounts.google.com/o/oauth2/auth?access_type=offline&client_id=abc&state=xyz")
        );
        assert_eq!(parse_antigravity_result("Logged in as me@gmail.com.\n", "", true), Ok(Some("me@gmail.com".into())));
        assert_eq!(parse_antigravity_result("", "", true), Ok(None));
        let err = parse_antigravity_result("\ncouldn't log in: token exchange failed: invalid_grant\n", "", false).unwrap_err();
        assert!(err.contains("token exchange failed"));
        let cancelled = parse_antigravity_result("\nlogin cancelled: no authorization code was entered\n", "", false).unwrap_err();
        assert!(cancelled.contains("login cancelled"));
    }

    #[test]
    fn parses_claude_osc8_url_feedback_and_completion() {
        let u = "https://claude.ai/oauth/authorize?code=true&state=abc";
        let out = format!(
            "Opening browser to sign in...\nIf the browser didn't open, visit:\n\x1b]8;;{u}\x07{u}\x1b]8;;\x07\nPaste code here > "
        );
        assert_eq!(extract_claude_url(&out).as_deref(), Some(u));
        let p = "https://platform.claude.com/oauth/authorize?s=1";
        let st_osc = format!("\x1b]8;;{p}\x1b\\{p}\x1b]8;;\x1b\\");
        assert_eq!(extract_claude_url(&st_osc).as_deref(), Some(p));
        assert!(is_claude_success_line("\x1b[32mLogin successful.\x1b[0m"));
        assert!(!is_claude_success_line("Opening browser to sign in..."));
        assert!(is_claude_feedback_line("That doesn't look like the full code. Copy all of it, including the # in the middle."));
        assert!(!is_claude_feedback_line("Couldn't log in: That code wasn't accepted."));
        assert!(!is_claude_feedback_line("   "));
    }

    #[test]
    fn parses_codex_device_auth_with_ansi_sgr() {
        let out = "1. Open this link in your browser and sign in to your account\n\
            \x1b[94m   https://auth.openai.com/codex/device\x1b[0m\n\n\
            2. Enter this one-time code (expires in 15 minutes)\n\
            \x1b[94m   ABCD-EFGHI\x1b[0m\n";
        let (url, code) = parse_codex_device_auth(out);
        assert_eq!(url.as_deref(), Some("https://auth.openai.com/codex/device"));
        assert_eq!(code.as_deref(), Some("ABCD-EFGHI"));
        assert_eq!(parse_codex_device_cli_error("", "Error logging in: device code expired\n"), "device code expired");
    }

    #[test]
    fn parses_codex_login_completed_notifications() {
        let ok = json!({"jsonrpc":"2.0","method":"account/login/completed","params":{"loginId":"id-1","success":true,"error":null}});
        assert_eq!(parse_codex_login_completed(&ok, "id-1"), Some(Ok(())));
        assert_eq!(parse_codex_login_completed(&ok, "id-2"), None);
        let fail = json!({"jsonrpc":"2.0","method":"account/login/completed","params":{"loginId":"id-1","success":false,"error":"denied"}});
        assert_eq!(parse_codex_login_completed(&fail, "id-1"), Some(Err("denied".into())));
        let other = json!({"jsonrpc":"2.0","method":"account/updated","params":{}});
        assert_eq!(parse_codex_login_completed(&other, "id-1"), None);
    }

    #[test]
    fn validates_codex_callback_url_strictly() {
        assert!(validate_codex_callback_url("http://127.0.0.1:1455/auth/callback?code=ac_123&scope=openid%20profile&state=s-1").is_ok());
        assert!(validate_codex_callback_url("  http://localhost:1457/auth/callback?code=abc&state=xyz  ").is_ok());
        for bad in [
            "",
            "https://127.0.0.1:1455/auth/callback?code=abc",
            "http://127.0.0.1:8080/auth/callback?code=abc",
            "http://evil.com:1455/auth/callback?code=abc",
            "http://127.0.0.1:1455/other?code=abc",
            "http://127.0.0.1:1455/auth/callback?state=only",
            "http://127.0.0.1:1455/auth/callback?code=",
            "http://127.0.0.1:1455/auth/callback?code=abc;id",
            "http://127.0.0.1:1455/auth/callback?code=$(whoami)",
            "http://127.0.0.1:1455/auth/callback?code=abc'&state=x",
            "http://127.0.0.1:1455/auth/callback?code=abc\nfoo",
            "http://127.0.0.1:1455/auth/callback?code=abc#frag",
            "http://127.0.0.1:1455/auth/callback?code=%ZZ",
        ] {
            assert!(validate_codex_callback_url(bad).is_err(), "expected reject for {bad:?}");
        }
    }

    #[test]
    fn resolves_codex_method_by_location() {
        let local = Location::Local;
        let wsl = Location::Wsl { distro: "Ubuntu".into() };
        let ssh = Location::Ssh { host: "mac".into() };
        assert_eq!(resolve_codex_method(&local, LoginMethod::Auto), CodexMode::Browser);
        assert_eq!(resolve_codex_method(&wsl, LoginMethod::Auto), CodexMode::Browser);
        assert_eq!(resolve_codex_method(&ssh, LoginMethod::Auto), CodexMode::Device);
        assert_eq!(resolve_codex_method(&local, LoginMethod::Device), CodexMode::Device);
        assert_eq!(resolve_codex_method(&ssh, LoginMethod::Browser), CodexMode::Browser);
    }

    #[test]
    fn serializes_login_types_in_camel_case() {
        let start = LoginStart {
            session_id: "s1".into(),
            agent: AgentKind::Codex,
            location: Location::Ssh { host: "mac".into() },
            kind: LoginPromptKind::Device,
            url: "https://auth.openai.com/codex/device".into(),
            user_code: Some("ABCD-EFGH".into()),
            auto_opened: false,
            input_label: None,
            hint: Some("Enter code".into()),
            can_switch_to_device: false,
            can_switch_to_browser: true,
        };
        let v = serde_json::to_value(&start).unwrap();
        assert_eq!(v["sessionId"], "s1");
        assert_eq!(v["kind"], "device");
        assert_eq!(v["userCode"], "ABCD-EFGH");
        assert_eq!(v["autoOpened"], false);
        assert_eq!(v["canSwitchToBrowser"], true);

        let fb = serde_json::to_value(LoginEvent::Feedback { session_id: "s1".into(), message: "retry".into() }).unwrap();
        assert_eq!(fb, json!({"kind": "feedback", "sessionId": "s1", "message": "retry"}));
        let done = serde_json::to_value(LoginEvent::Done {
            session_id: "s1".into(),
            account: Account { signed_in: true, label: Some("me@x.dev".into()), plan: Some("pro".into()), error: None },
        })
        .unwrap();
        assert_eq!(done["kind"], "done");
        assert_eq!(done["sessionId"], "s1");
        assert_eq!(done["account"]["signedIn"], true);
    }

    #[test]
    fn formats_claude_post_login_errors() {
        let acct = Account::default();
        let locked = claude_post_login_error(&acct, true);
        assert!(locked.contains("security unlock-keychain ~/Library/Keychains/login.keychain-db"));
        assert!(locked.contains("CLAUDE_CODE_OAUTH_TOKEN"));

        let with_err = Account { error: Some("keychain read failed".into()), ..Default::default() };
        assert!(claude_post_login_error(&with_err, false).contains("keychain read failed"));

        let generic = claude_post_login_error(&acct, false);
        assert!(generic.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    }
}
