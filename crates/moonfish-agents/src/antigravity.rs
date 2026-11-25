//! Google Antigravity's CLI (`agy`): account, model and session integration.
//!
//! - Chats run over ACP (`agy acp`, see [`crate::acp`]).
//! - Models: `agy models` prints `id<TAB>name` once per effort (`claude-opus-5-5-high`,
//!   "Claude Opus 5.5 (High)"); a session's model option has the base model and a separate
//!   thinking level, so a model with more than one effort is listed once, with its efforts.
//! - Sign-in: `agy acp --login` in a terminal. Who: the CLI's OAuth token, which names the
//!   account (`id_token_email`): the file `~/.gemini/antigravity-cli/antigravity-oauth-token`
//!   (in WSL, over SSH, or without a keyring), else Windows Credential Manager (`gemini:antigravity`)
//!   or the Linux keyring (`secret-tool`, service `gemini`, user `antigravity`). Only the email is
//!   read out of it; the token never leaves the CLI's machine.
//! - Limits: `agy -p /usage --output-format json`, a built-in command (1.1.11 and newer), not a
//!   prompt: per model group, a weekly and a 5-hour window.
//! - One-off questions: `agy --print= --sandbox` with stream-json in and out.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use moonfish_core::settings::{EnvVar, Location};
use moonfish_transport::Spec;
use serde_json::{json, Value};

use crate::account::{Account, LimitWindow, Limits};
use crate::driver::{DriverResult, StartOptions};
use crate::event::{ModelOption, SlashCommand};

pub const NOT_SIGNED_IN: &str =
    "Antigravity isn't signed in. Run `agy` in a terminal or open Settings, Providers, Antigravity, and choose Sign in.";

pub fn not_signed_in(location: &Location) -> String {
    match location {
        Location::Local => NOT_SIGNED_IN.to_string(),
        Location::Wsl { distro } => format!(
            "Antigravity isn't signed in in {distro}. Run `agy acp --login` in {distro}, or open Settings, Projects, and sign in there."
        ),
        Location::Ssh { host } => format!(
            "Antigravity isn't signed in for SSH sessions on {host}. Over SSH, agy reads \
             ~/.gemini/antigravity-cli/antigravity-oauth-token instead of the desktop keyring. Run `agy acp --login` over SSH or \
             `AGY_CLI_FORCE_FILE_TOKEN_STORAGE=1 agy acp --login` on {host}."
        ),
    }
}

fn is_auth_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("unauthenticated")
        || lower.contains("authentication required")
        || lower.contains("not logged in")
        || lower.contains("please sign in")
        || lower.contains("401")
}

/// Effort suffixes `agy` puts on model ids, lowest first.
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Chats need `session/resume` and `session/list` in `agy acp` (1.5.2).
pub const MINIMUM: &str = "1.5.2";
/// Older `agy` takes `/usage` in print mode as a prompt (and bills it).
const USAGE_MINIMUM: &str = "1.1.11";

fn var(key: &str, value: &str) -> EnvVar {
    EnvVar { key: key.into(), value: value.into(), in_keychain: false }
}

/// The environment every `agy` run gets: no self-update mid-run (it wants the word `true`), and
/// no Gemini API key from the surroundings, which switches `agy` to key sign-in that its models
/// refuse (401). A key set in Settings → Providers → Antigravity is kept.
pub fn acp_env(env: &mut Vec<EnvVar>) {
    let set = |env: &Vec<EnvVar>, k: &str| env.iter().any(|e| e.key == k);
    if !set(env, "AGY_CLI_DISABLE_AUTO_UPDATE") {
        env.push(var("AGY_CLI_DISABLE_AUTO_UPDATE", "true"));
    }
    for k in ["GEMINI_API_KEY", "GOOGLE_API_KEY"] {
        if !set(env, k) {
            env.push(var(k, ""));
        }
    }
}

/// Environment for `agy` runs on `location`: [`acp_env`] plus file token storage for SSH and WSL
/// so remote runs read the token written by `agy acp --login`.
pub fn acp_env_for(location: &Location, env: &mut Vec<EnvVar>) {
    acp_env(env);
    if location.is_remote() && !env.iter().any(|e| e.key == "AGY_CLI_FORCE_FILE_TOKEN_STORAGE") {
        env.push(var("AGY_CLI_FORCE_FILE_TOKEN_STORAGE", "1"));
    }
}

fn program(binary: &str) -> &str {
    if binary.trim().is_empty() {
        "agy"
    } else {
        binary.trim()
    }
}

fn home_cwd(location: &Location) -> String {
    if matches!(location, Location::Local) {
        moonfish_core::paths::home_dir().to_string_lossy().into_owned()
    } else {
        "~".into()
    }
}

// ---- Sign-in ---------------------------------------------------------------------------------

/// Who `agy` is signed in as on `location`, from its own token store.
pub fn account(location: &Location, env: &[EnvVar]) -> Account {
    let found = match location {
        Location::Local => local_token_email(),
        remote => remote_token_email(remote, env),
    };
    match found {
        Ok(Some(Found::Token(email))) => Account { signed_in: true, label: email.or(Some("Google".into())), plan: None, error: None },
        Ok(Some(Found::ApiKey)) => Account { signed_in: true, label: Some("API key".into()), plan: None, error: None },
        Ok(None) => Account { signed_in: false, label: None, plan: None, error: None },
        Err(e) => Account { error: Some(e), ..Default::default() },
    }
}

#[derive(Debug, PartialEq)]
enum Found {
    /// An OAuth sign-in, and its email when the token names it.
    Token(Option<String>),
    /// `settings.json` picks a model provider: an API key, not a Google sign-in.
    ApiKey,
}

/// The token's account email: `id_token_email`, or the verified `email` in its `id_token`.
fn email_of(token: &str) -> Option<String> {
    let v: Value = serde_json::from_str(token.trim()).ok()?;
    if let Some(e) = v["id_token_email"].as_str().filter(|e| e.contains('@')) {
        return Some(e.to_string());
    }
    let jwt = v["id_token"].as_str().or(v["token"]["id_token"].as_str())?;
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(jwt.split('.').nth(1)?.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&payload).ok()?;
    (claims["email_verified"] == true).then(|| claims["email"].as_str().map(String::from)).flatten()
}

/// A keyring value as go-keyring stores it: `go-keyring-base64:…`, `go-keyring-encoded:<hex>`,
/// or the JSON itself.
fn keyring_value(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if let Some(b) = raw.strip_prefix("go-keyring-base64:") {
        use base64::Engine;
        return base64::engine::general_purpose::STANDARD.decode(b).ok().and_then(|v| String::from_utf8(v).ok());
    }
    if let Some(h) = raw.strip_prefix("go-keyring-encoded:") {
        let bytes: Option<Vec<u8>> = (0..h.len()).step_by(2).map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok()).collect();
        return bytes.and_then(|b| String::from_utf8(b).ok());
    }
    Some(raw.to_string())
}

/// `~/.gemini/antigravity-cli/settings.json` names a model provider (an API key), or is
/// unreadable JSON (as `agy` reads it then).
fn uses_api_key(settings: &str) -> bool {
    match serde_json::from_str::<Value>(settings) {
        Ok(v) => v["modelProvider"].as_str().is_some_and(|p| !p.is_empty()) || v["modelProvider"] == true,
        Err(_) => !settings.trim().is_empty(),
    }
}

fn local_token_email() -> Result<Option<Found>, String> {
    let dir = moonfish_core::paths::home_dir().join(".gemini").join("antigravity-cli");
    if std::fs::read_to_string(dir.join("settings.json")).is_ok_and(|s| uses_api_key(&s)) {
        return Ok(Some(Found::ApiKey));
    }
    if let Ok(t) = std::fs::read_to_string(dir.join("antigravity-oauth-token")) {
        if !t.trim().is_empty() {
            return Ok(Some(Found::Token(email_of(&t))));
        }
    }
    Ok(keyring_token()?.map(|t| Found::Token(email_of(&t))))
}

/// The token in the OS keyring, where `agy` keeps it on a desktop.
#[cfg(windows)]
fn keyring_token() -> Result<Option<String>, String> {
    let entry = keyring::Entry::new_with_target("gemini:antigravity", "gemini", "antigravity").map_err(|e| e.to_string())?;
    match entry.get_secret() {
        Ok(bytes) => Ok(String::from_utf8(bytes).ok().and_then(|s| keyring_value(&s))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("couldn't read Windows Credential Manager: {e}")),
    }
}

#[cfg(target_os = "linux")]
fn keyring_token() -> Result<Option<String>, String> {
    // `secret-tool` (libsecret) talks to the desktop's keyring; none installed, or no keyring
    // running, means `agy` uses its file, read above.
    let mut cmd = std::process::Command::new("secret-tool");
    cmd.args(["lookup", "service", "gemini", "username", "antigravity"]).stdin(std::process::Stdio::null());
    match cmd.output() {
        Ok(o) if o.status.success() => {
            Ok(String::from_utf8(o.stdout).ok().filter(|s| !s.trim().is_empty()).and_then(|s| keyring_value(&s)))
        }
        _ => Ok(None),
    }
}

#[cfg(target_os = "macos")]
fn keyring_token() -> Result<Option<String>, String> {
    let o = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", "gemini", "-a", "antigravity", "-w"])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned()).and_then(|s| keyring_value(&s)))
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn keyring_token() -> Result<Option<String>, String> {
    Ok(None)
}

/// In WSL or over SSH `agy` keeps its token in the file. Only the email comes back.
const REMOTE_ACCOUNT: &str = r#"d="${HOME}/.gemini/antigravity-cli"
if [ -f "$d/settings.json" ] && grep -Eq '"modelProvider"[[:space:]]*:[[:space:]]*(true|"[^"])' "$d/settings.json"; then echo api-key; exit 0; fi
f="$d/antigravity-oauth-token"
[ -s "$f" ] || { echo none; exit 0; }
e=$(grep -o '"id_token_email"[[:space:]]*:[[:space:]]*"[^"]*"' "$f" | head -n 1 | sed 's/.*"\([^"]*\)"$/\1/')
echo "token $e""#;

fn remote_token_email(location: &Location, env: &[EnvVar]) -> Result<Option<Found>, String> {
    let args = vec!["-c".into(), REMOTE_ACCOUNT.into()];
    let out = moonfish_transport::run(&Spec { location, cwd: "~", program: "sh", args, env }, None, Duration::from_secs(20))?;
    if !out.ok() {
        return Err(out.stderr.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("couldn't read the sign-in").to_string());
    }
    Ok(parse_remote_account(&out.stdout))
}

fn parse_remote_account(out: &str) -> Option<Found> {
    let line = out.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    match line {
        "api-key" => Some(Found::ApiKey),
        "none" => None,
        l => l.strip_prefix("token").map(|e| Found::Token(Some(e.trim().to_string()).filter(|e| e.contains('@')))),
    }
}

// ---- Limits ----------------------------------------------------------------------------------

static USAGE_BILLED_AS_PROMPT_LOCATIONS: Mutex<Vec<Location>> = Mutex::new(Vec::new());

/// The plan's windows on `location`, from `agy`'s `/usage`.
pub fn limits(binary: &str, location: &Location, env: &[EnvVar]) -> Limits {
    let fail = |e: String| Limits { error: Some(e), ..Default::default() };
    if USAGE_BILLED_AS_PROMPT_LOCATIONS.lock().unwrap().contains(location) {
        return fail(
            "This Antigravity CLI answers /usage as a prompt, so Moonfish stopped asking. Run `agy update` and restart Moonfish.".into(),
        );
    }
    let prog = program(binary);
    let cwd = home_cwd(location);
    let mut env = env.to_vec();
    acp_env_for(location, &mut env);
    let run = |args: &[&str], limit: u64| {
        let args = args.iter().map(|a| a.to_string()).collect();
        moonfish_transport::run(&Spec { location, cwd: &cwd, program: prog, args, env: &env }, None, Duration::from_secs(limit))
    };
    match run(&["--version"], 10) {
        Ok(o) if o.ok() => {
            if let Some(v) = crate::probe::parse_version(&o.stdout).filter(|v| crate::probe::older(v, USAGE_MINIMUM)) {
                return fail(format!("Antigravity {v} can't report usage. Run `agy update` to update to {USAGE_MINIMUM} or newer."));
            }
        }
        Ok(o) if o.code == Some(127) => return Limits { missing: true, ..Default::default() },
        Ok(_) => {}
        Err(e) => return fail(e),
    }
    let out = match run(&["-p", "/usage", "--output-format", "json", "--print-timeout", "20s"], 35) {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    match parse_usage(&out.stdout, &out.stderr) {
        Ok(windows) => Limits { windows, ..Default::default() },
        Err(UsageError::Prompted) => {
            USAGE_BILLED_AS_PROMPT_LOCATIONS.lock().unwrap().push(location.clone());
            fail(
                "This Antigravity CLI answered /usage as a prompt instead of a command, so Moonfish stopped asking. \
                 Run `agy update` and restart Moonfish."
                    .into(),
            )
        }
        Err(UsageError::Other(e)) if e == NOT_SIGNED_IN => fail(not_signed_in(location)),
        Err(UsageError::Other(e)) => fail(e),
    }
}

#[derive(Debug, PartialEq)]
pub enum UsageError {
    /// It ran a model turn for it.
    Prompted,
    Other(String),
}

/// `/usage`'s JSON line → windows: per model group, its 5-hour and weekly buckets, as
/// "<group> · 5 hours" and "<group> · this week" (`remaining_fraction` turned into used).
/// Unmetered (`disabled`) buckets are left out.
pub fn parse_usage(stdout: &str, stderr: &str) -> Result<Vec<LimitWindow>, UsageError> {
    let lines: Vec<Value> = stdout.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).collect();
    if let Some(v) = lines.iter().find(|v| v["command"]["name"] == "usage" || v["command"]["data"]["groups"].is_array()) {
        let mut out = Vec::new();
        for g in v["command"]["data"]["groups"].as_array().into_iter().flatten() {
            let group = g["name"].as_str().unwrap_or("").trim();
            let group = group.strip_suffix(" Models").or(group.strip_suffix(" models")).unwrap_or(group);
            let mut buckets: Vec<&Value> = g["buckets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|b| b["disabled"] != true && b["remaining_fraction"].is_number())
                .collect();
            // The short window first, as the other agents list theirs.
            buckets.sort_by_key(|b| {
                if b["window"] == "5h" {
                    0
                } else if b["window"] == "weekly" {
                    1
                } else {
                    2
                }
            });
            for b in buckets {
                let window = match b["window"].as_str() {
                    Some("5h") => "5 hours".to_string(),
                    Some("weekly") => "this week".to_string(),
                    _ => b["name"].as_str().unwrap_or("limit").to_string(),
                };
                let used = (1.0 - b["remaining_fraction"].as_f64().unwrap_or(1.0)) * 100.0;
                out.push(LimitWindow {
                    label: if group.is_empty() { window } else { format!("{group} · {window}") },
                    used_percent: used.round().clamp(0.0, 100.0),
                    resets_at: crate::account::reset_ms(&b["reset_time"]),
                });
            }
        }
        return Ok(out);
    }
    if lines.iter().any(|v| v["num_turns"].as_u64().is_some_and(|n| n > 0) || v["conversation_id"].as_str().is_some_and(|c| !c.is_empty()))
    {
        return Err(UsageError::Prompted);
    }
    let text = format!(
        "{stdout}
{stderr}"
    );
    let lower = text.to_lowercase();
    let status = lines.iter().find_map(|v| v["error"].as_str().map(String::from));
    Err(UsageError::Other(if is_auth_error(&text) {
        NOT_SIGNED_IN.to_string()
    } else if lower.contains("permission_denied") || lower.contains("403") {
        "Antigravity says this account has no plan with usage limits.".to_string()
    } else if lower.contains("resource_exhausted") || lower.contains("429") {
        "Antigravity is rate limiting requests; try again in a little while.".to_string()
    } else {
        status
            .or_else(|| text.lines().map(str::trim).rfind(|l| !l.is_empty()).map(String::from))
            .unwrap_or_else(|| "Antigravity didn't report usage.".into())
    }))
}

// ---- One-off questions -----------------------------------------------------------------------

/// The quick model for one-off questions: Flash at its lowest effort.
pub fn pick_cheap(models: &[ModelOption]) -> Option<(String, Option<String>, String)> {
    let m = models.iter().find(|m| m.id.contains("flash")).or(models.first())?;
    let effort = m.efforts.first().cloned();
    let label = match &effort {
        Some(e) => format!("{} ({e})", m.name),
        None => m.name.clone(),
    };
    Some((m.id.clone(), effort, label))
}

/// Arguments for a one-off question: print mode, stream-json in and out, sandboxed commands.
pub fn oneshot_args(model: Option<&str>, effort: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> =
        ["--print=", "--input-format=stream-json", "--output-format=stream-json", "--sandbox"].map(String::from).to_vec();
    if let Some(m) = model {
        a.extend(["--model".into(), m.into()]);
    }
    if let Some(e) = effort {
        a.extend(["--effort".into(), e.into()]);
    }
    a
}

/// The question as `agy`'s stream-json input.
pub fn oneshot_input(question: &str) -> String {
    format!(
        "{}
",
        json!({"event": "user", "message": {"role": "user", "content": [{"type": "text", "text": question}]}})
    )
}

/// The answer from the final `result` event.
pub fn oneshot_answer(stdout: &str) -> Result<String, String> {
    for line in stdout.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
        if v["event"] == "result" {
            let r = &v["result"];
            return match r["status"].as_str() {
                Some("SUCCESS") => Ok(r["response"].as_str().unwrap_or("").to_string()),
                _ => Err(r["error"].as_str().or(r["status"].as_str()).unwrap_or("no answer").to_string()),
            };
        }
    }
    Err("Antigravity gave no answer".into())
}

fn clean_line(line: &str) -> String {
    let line = line.trim_end_matches('\r');
    let line = line.rsplit('\r').next().unwrap_or(line);
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
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

fn split_model_line(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    if let Some((id, name)) = trimmed.split_once('\t') {
        let (id, name) = (id.trim(), name.trim());
        if !id.is_empty() && !name.is_empty() && !id.contains(char::is_whitespace) {
            return Some((id, name));
        }
        return None;
    }
    let (id, name) = trimmed.split_once("  ")?;
    let (id, name) = (id.trim(), name.trim());
    if id.is_empty() || name.is_empty() || id.contains(char::is_whitespace) || id.ends_with(':') {
        return None;
    }
    Some((id, name))
}

/// `agy models`: one `id<TAB>name` (or space-padded columns on a TTY) line per model and effort.
pub fn parse_models(out: &str) -> Vec<ModelOption> {
    let mut models: Vec<ModelOption> = Vec::new();
    for raw in out.lines() {
        let cleaned = clean_line(raw);
        let Some((id, name)) = split_model_line(&cleaned) else { continue };
        // `claude-opus-5-5-high` / "Claude Opus 5.5 (High)" → base `claude-opus-5-5`, effort high.
        let split = EFFORTS.iter().find_map(|e| {
            let base = id.strip_suffix(e)?.strip_suffix('-')?;
            let shown = name.strip_suffix(')').and_then(|n| n.rsplit_once(" (")).map(|(n, _)| n).unwrap_or(name);
            Some((base, shown, *e))
        });
        match split {
            Some((base, shown, effort)) => match models.iter_mut().find(|m| m.id == base) {
                Some(m) => {
                    if !m.efforts.iter().any(|existing| existing.as_str() == effort) {
                        m.efforts.push(effort.to_string());
                    }
                }
                None => models.push(ModelOption {
                    id: base.to_string(),
                    name: shown.to_string(),
                    efforts: vec![effort.to_string()],
                    // The full id and name, for a model that turns out to have one effort.
                    description: format!("{id}\t{name}"),
                    ..Default::default()
                }),
            },
            None => models.push(ModelOption { id: id.to_string(), name: name.to_string(), ..Default::default() }),
        }
    }
    // A model with one effort is that one id (as `agy`'s own picker lists it).
    for m in models.iter_mut() {
        if m.efforts.len() == 1 {
            if let Some((id, name)) = m.description.split_once('\t') {
                m.id = id.to_string();
                m.name = name.to_string();
            }
            m.efforts.clear();
        }
        m.efforts.sort_by_key(|e| EFFORTS.iter().position(|x| x == e).unwrap_or(99));
        m.description.clear();
    }
    models
}

/// The models `agy` offers where it runs.
pub fn models(binary: &str, location: &Location, cwd: &str, env: &[EnvVar]) -> DriverResult<Vec<ModelOption>> {
    let program = program(binary);
    let mut env = env.to_vec();
    acp_env_for(location, &mut env);
    let out =
        moonfish_transport::run(&Spec { location, cwd, program, args: vec!["models".into()], env: &env }, None, Duration::from_secs(45))?;
    if !out.ok() {
        if out.code == Some(127) {
            return Err(format!("{program} isn't installed there"));
        }
        let all = format!("{}\n{}", out.stdout, out.stderr);
        if is_auth_error(&all) {
            return Err(not_signed_in(location));
        }
        let why = out.stderr.lines().chain(out.stdout.lines()).rev().find_map(|l| {
            let cleaned = clean_line(l);
            let trimmed = cleaned.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
        return Err(why.unwrap_or_else(|| "agy models failed".into()));
    }
    let models = parse_models(&out.stdout);
    if models.is_empty() {
        let all = format!("{}\n{}", out.stdout, out.stderr);
        let empty_or_fetching = out.stdout.lines().all(|l| {
            let cleaned = clean_line(l);
            let trimmed = cleaned.trim();
            trimmed.is_empty() || trimmed.starts_with("Fetching available models")
        });
        if is_auth_error(&all) || empty_or_fetching {
            return Err(not_signed_in(location));
        }
        return Err("Antigravity returned models in an unexpected format.".into());
    }
    Ok(models)
}

/// `agy`'s commands before a chat's first `available_commands_update`. Its skills come once a
/// chat runs.
pub fn commands() -> Vec<SlashCommand> {
    let c = |name: &str, description: &str| SlashCommand {
        name: name.into(),
        description: description.into(),
        hint: String::new(),
        skill: false,
    };
    vec![
        c("goal", "Run until the goal is finished."),
        c("compact", "Summarize conversation history to free up the context window."),
        c("schedule", "Run an instruction on a recurring schedule or as a one-time timer."),
        c("browser", "Run a browser agent for web tasks."),
        c("plan", "Plan the steps before executing a task."),
        c("grill-me", "Interview me to align on a plan."),
        c("teamwork", "Run a team of agents on large projects."),
        c("swarm", "Run a swarm of agents working together on a task."),
        c("learn", "Reflect on recent successes or corrections to capture reusable skills or rules."),
        c("boost", "Run the Boost multi-agent orchestrator for complex tasks."),
    ]
}

/// The models and commands for a chat that hasn't started. From `agy models`, not a session:
/// `agy` keeps every session it opens as a conversation, even an empty one. Local skills are
/// discovered read-only from Antigravity's workspace and user skill roots until the live session
/// replaces them with `available_commands_update`.
pub fn catalog(opts: &StartOptions) -> DriverResult<(Vec<ModelOption>, Vec<SlashCommand>)> {
    let models = models(&opts.binary, &opts.location, &opts.cwd, &opts.env)?;
    let mut cmds = commands();
    if matches!(opts.location, Location::Local) {
        let mut seen: HashSet<String> = cmds.iter().map(|c| c.name.clone()).collect();
        for skill in discover_skills(Path::new(&opts.cwd), &moonfish_core::paths::home_dir()) {
            if seen.insert(skill.name.clone()) {
                cmds.push(skill);
            }
        }
    }
    Ok((models, cmds))
}

/// Read-only scan of Antigravity's local skill roots: `<cwd>/.agents/skills/*/SKILL.md` and
/// `<cwd>/.agent/skills/*/SKILL.md` walking up to the repo root, then `~/.agents/skills/*/SKILL.md`
/// and `~/.gemini/config/skills/*/SKILL.md`. Closer workspace skills win on name collision.
pub fn discover_skills(cwd: &Path, home: &Path) -> Vec<SlashCommand> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        roots.push(d.join(".agents").join("skills"));
        roots.push(d.join(".agent").join("skills"));
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    roots.push(home.join(".agents").join("skills"));
    roots.push(home.join(".gemini").join("config").join("skills"));

    let mut seen_roots: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut skills = Vec::new();
    for root in roots {
        if !seen_roots.insert(root.clone()) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&skill_md) else { continue };
            let fallback = entry.file_name().to_string_lossy().into_owned();
            if let Some(cmd) = parse_skill_frontmatter(&text, &fallback) {
                if seen_names.insert(cmd.name.clone()) {
                    skills.push(cmd);
                }
            }
        }
    }
    skills
}

fn parse_skill_frontmatter(text: &str, fallback_name: &str) -> Option<SlashCommand> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut front = Vec::new();
    let mut closed = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "..." {
            closed = true;
            break;
        }
        front.push(line);
    }
    if !closed {
        return None;
    }
    let mut name = String::new();
    let mut description = String::new();
    let mut i = 0;
    while i < front.len() {
        let line = front[i];
        if !line.starts_with([' ', '\t']) {
            if let Some((k, raw)) = line.split_once(':') {
                let key = k.trim();
                if key == "name" || key == "description" {
                    let val = raw.trim();
                    let parsed = if val.is_empty() || matches!(val, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
                        let mut block = Vec::new();
                        while i + 1 < front.len() && (front[i + 1].starts_with([' ', '\t']) || front[i + 1].trim().is_empty()) {
                            i += 1;
                            let part = front[i].trim();
                            if !part.is_empty() {
                                block.push(part);
                            }
                        }
                        block.join(" ")
                    } else {
                        unquote_yaml(val)
                    };
                    if key == "name" {
                        name = parsed;
                    } else {
                        description = parsed;
                    }
                }
            }
        }
        i += 1;
    }
    let name = name.trim().trim_start_matches('/').to_string();
    let name = if name.is_empty() { fallback_name.trim().trim_start_matches('/').to_string() } else { name };
    if name.is_empty() {
        return None;
    }
    let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(SlashCommand { name, description, hint: String::new(), skill: true })
}

fn unquote_yaml(s: &str) -> String {
    if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
        return inner.replace("''", "'");
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const LISTING: &str = "Fetching available models...\n\
gemini-4-argon-medium\tGemini 4 Argon (Medium)\n\
gemini-4-argon-high\tGemini 4 Argon (High)\n\
gemini-4-argon-low\tGemini 4 Argon (Low)\n\
claude-opus-5-5-low\tClaude Opus 5.5 (Low)\n\
claude-opus-5-5-high\tClaude Opus 5.5 (High)\n\
gpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n";

    #[test]
    fn groups_efforts_under_their_model() {
        let m = parse_models(LISTING);
        let ids: Vec<&str> = m.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["gemini-4-argon", "claude-opus-5-5", "gpt-oss-120b-medium"]);
        assert_eq!(m[0].name, "Gemini 4 Argon");
        assert_eq!(m[0].efforts, ["low", "medium", "high"]);
        assert_eq!(m[1].efforts, ["low", "high"]);
        // One effort: kept whole, as the session's model option has it.
        assert_eq!((m[2].name.as_str(), m[2].efforts.len()), ("GPT-OSS 120B (Medium)", 0));
        assert!(m.iter().all(|m| m.description.is_empty()));
    }

    /// `agy -p /usage --output-format json` from 1.5.3 (Google AI Ultra, nothing used yet).
    const USAGE: &str = r##"{"conversation_id":"","status":"SUCCESS","response":"Gemini Models\tWeekly Limit Remaining\t100%\t2026-11-16T16:14:12Z\nGemini Models\tFive Hour Limit Remaining\t100%\t2026-11-09T21:14:12Z\nClaude and GPT models\tWeekly Limit Remaining\t100%\t2026-11-16T16:14:12Z\nClaude and GPT models\tFive Hour Limit Remaining\t100%\t2026-11-09T21:14:12Z\n","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"command":{"name":"usage","data":{"description":"Within each group, models share a weekly limit and a 5-hour limit. Quota is consumed proportionally to the cost of the tokens. Thus, limits will last longer with shorter tasks or using more cost-effective models. The 5-hour limit smooths out aggregate demand to fairly distribute global capacity across all users, while your weekly limit is tied directly to your individual tier.","groups":[{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-16T16:14:12Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":1,"reset_time":"2026-11-09T21:14:12Z"}]},{"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-16T16:14:12Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":1,"reset_time":"2026-11-09T21:14:12Z"}]}]}}}"##;

    #[test]
    fn reads_usage_windows() {
        let w = parse_usage(USAGE, "").unwrap();
        let labels: Vec<&str> = w.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["Gemini · 5 hours", "Gemini · this week", "Claude and GPT · 5 hours", "Claude and GPT · this week"]);
        assert!(w.iter().all(|w| w.used_percent == 0.0));
        assert_eq!(w[0].resets_at, Some(1_794_258_852_000)); // 2026-11-09T21:14:12Z
                                                             // Used is what isn't left; unmetered buckets are left out.
        let some = USAGE.replacen("\"remaining_fraction\":1,", "\"remaining_fraction\":0.27,", 1).replacen(
            "\"remaining_fraction\":1,",
            "\"disabled\":true,\"remaining_fraction\":1,",
            1,
        );
        let w = parse_usage(&some, "").unwrap();
        assert_eq!((w.len(), w[0].label.as_str(), w[0].used_percent), (3, "Gemini · this week", 73.0));
    }

    #[test]
    fn usage_problems_in_plain_words() {
        let billed = r#"{"conversation_id":"c-1","status":"SUCCESS","response":"Your usage is…","num_turns":1}"#;
        assert_eq!(parse_usage(billed, "").unwrap_err(), UsageError::Prompted);
        let out = r#"AGY_ERROR:{"status":"UNAUTHENTICATED","error_code":401}"#;
        assert_eq!(parse_usage(out, "").unwrap_err(), UsageError::Other(NOT_SIGNED_IN.into()));
        let denied = r#"{"status":"ERROR","error":"PERMISSION_DENIED: no subscription"}"#;
        assert!(matches!(parse_usage(denied, ""), Err(UsageError::Other(e)) if e.contains("no plan")));
        assert!(matches!(parse_usage("", "boom"), Err(UsageError::Other(e)) if e == "boom"));
    }

    #[test]
    fn reads_who_from_the_token() {
        let file = r#"{"token":{"access_token":"a","token_type":"Bearer","refresh_token":"r","expiry":"2026-11-09T11:08:22-06:00"},"auth_method":"consumer","id_token_email":"me@gmail.com"}"#;
        assert_eq!(email_of(file).as_deref(), Some("me@gmail.com"));
        // Else the verified email in the id_token.
        use base64::Engine;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"iss":"https://accounts.google.com","email":"you@x.dev","email_verified":true}"#);
        assert_eq!(email_of(&format!(r#"{{"token":{{"id_token":"h.{claims}.s"}}}}"#)).as_deref(), Some("you@x.dev"));
        let unverified = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"email":"you@x.dev","email_verified":false}"#);
        assert_eq!(email_of(&format!(r#"{{"id_token":"h.{unverified}.s"}}"#)), None);
        // go-keyring's encodings.
        let b64 = base64::engine::general_purpose::STANDARD.encode(file);
        assert_eq!(keyring_value(&format!("go-keyring-base64:{b64}")).as_deref(), Some(file));
        let hex: String = "{}".bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(keyring_value(&format!("go-keyring-encoded:{hex}")).as_deref(), Some("{}"));
        // API keys, and what the WSL / SSH script prints.
        assert!(uses_api_key(r#"{"modelProvider":"gemini"}"#));
        assert!(!uses_api_key(r#"{"model":"Gemini 4 Argon (Medium)"}"#));
        assert_eq!(parse_remote_account("token me@gmail.com\n"), Some(Found::Token(Some("me@gmail.com".into()))));
        assert_eq!(parse_remote_account("token \n"), Some(Found::Token(None)));
        assert_eq!(parse_remote_account("none\n"), None);
        assert_eq!(parse_remote_account("api-key"), Some(Found::ApiKey));
    }

    #[test]
    fn one_off_questions() {
        let m = parse_models(LISTING);
        assert_eq!(pick_cheap(&m), Some(("gemini-4-argon".into(), Some("low".into()), "Gemini 4 Argon (low)".into())));
        let flash = parse_models("gemini-3.8-flash-high\tGemini 3.8 Flash (High)\ngemini-3.8-flash-low\tGemini 3.8 Flash (Low)\n");
        let m: Vec<ModelOption> = parse_models(LISTING).into_iter().chain(flash).collect();
        assert_eq!(pick_cheap(&m).unwrap().0, "gemini-3.8-flash");
        assert_eq!(oneshot_args(Some("gemini-3.8-flash"), Some("low"))[4..], ["--model", "gemini-3.8-flash", "--effort", "low"]);
        let input: Value = serde_json::from_str(oneshot_input("Hi").trim()).unwrap();
        assert_eq!(input["message"]["content"][0]["text"], "Hi");
        // From agy 1.5.3.
        let out = r#"{"event": "init", "conversation_id": "7207", "init": {"model": "gemini-3.8-flash"}}
{"event": "step_update", "step_update": {"step_index": 1, "state": "ACTIVE", "step_type": "agent_response", "text_delta": "OK"}}
{"event": "result", "result": {"conversation_id": "7207", "status": "SUCCESS", "response": "OK\n", "num_turns": 1}}"#;
        assert_eq!(oneshot_answer(out).as_deref(), Ok("OK\n"));
        assert!(oneshot_answer("").is_err());
    }

    #[test]
    fn clears_a_stray_api_key_but_keeps_yours() {
        let mut env = vec![];
        acp_env(&mut env);
        let get = |env: &Vec<EnvVar>, k: &str| env.iter().find(|e| e.key == k).map(|e| e.value.clone());
        assert_eq!(get(&env, "AGY_CLI_DISABLE_AUTO_UPDATE").as_deref(), Some("true"));
        assert_eq!(get(&env, "GEMINI_API_KEY").as_deref(), Some(""));
        let mut env = vec![EnvVar { key: "GEMINI_API_KEY".into(), value: "k".into(), in_keychain: false }];
        acp_env(&mut env);
        assert_eq!(env.iter().filter(|e| e.key == "GEMINI_API_KEY").count(), 1);
        assert_eq!(get(&env, "GEMINI_API_KEY").as_deref(), Some("k"));

        let mut local = vec![];
        acp_env_for(&Location::Local, &mut local);
        assert_eq!(get(&local, "AGY_CLI_FORCE_FILE_TOKEN_STORAGE"), None);

        let mut ssh = vec![];
        acp_env_for(&Location::Ssh { host: "mac".into() }, &mut ssh);
        assert_eq!(get(&ssh, "AGY_CLI_FORCE_FILE_TOKEN_STORAGE").as_deref(), Some("1"));

        let mut wsl = vec![];
        acp_env_for(&Location::Wsl { distro: "Ubuntu".into() }, &mut wsl);
        assert_eq!(get(&wsl, "AGY_CLI_FORCE_FILE_TOKEN_STORAGE").as_deref(), Some("1"));

        let mut custom = vec![EnvVar { key: "AGY_CLI_FORCE_FILE_TOKEN_STORAGE".into(), value: "0".into(), in_keychain: false }];
        acp_env_for(&Location::Ssh { host: "mac".into() }, &mut custom);
        assert_eq!(get(&custom, "AGY_CLI_FORCE_FILE_TOKEN_STORAGE").as_deref(), Some("0"));
    }

    /// Against the real `agy` here (ignored): who's signed in, the plan's windows, the models.
    #[test]
    #[ignore]
    fn real_account_limits_models() {
        let loc = Location::Local;
        let a = account(&loc, &[]);
        eprintln!("account: signed_in={} label={:?} error={:?}", a.signed_in, a.label, a.error);
        assert!(a.signed_in);
        let l = limits("", &loc, &[]);
        eprintln!("limits: error={:?} missing={}", l.error, l.missing);
        for w in &l.windows {
            eprintln!("  {} {}% resets {:?}", w.label, w.used_percent, w.resets_at);
        }
        assert_eq!(l.windows.len(), 4);
        let m = models("", &loc, "/tmp", &[]).unwrap();
        eprintln!("models: {}", m.iter().map(|m| format!("{} {:?}", m.id, m.efforts)).collect::<Vec<_>>().join(", "));
        assert!(m.iter().any(|m| m.efforts.len() > 1));
    }

    #[test]
    fn nothing_listed_is_nothing() {
        assert!(parse_models("Fetching available models...\n").is_empty());
        let signed_out = "\rError: Please sign in to view available models. Launch the CLI without arguments to sign in.\n";
        assert!(parse_models(signed_out).is_empty());
    }

    #[test]
    fn parses_tty_ansi_crlf_and_real_agy_models_output() {
        let real = "gemini-4-neon-high\tGemini 4 Neon (High)\r\n\
gemini-4-neon-xhigh\tGemini 4 Neon (XHigh)\r\n\
gemini-4-neon-medium\tGemini 4 Neon (Medium)\r\n\
gemini-4-neon-low\tGemini 4 Neon (Low)\r\n\
gemini-pro-orchestrator-neon\tOrchestrator (Neon)\r\n\
gemini-pro-orchestrator\tOrchestrator (Carbon)\r\n\
claude-opus-5-5-low\tClaude Opus 5.5 (Low)\r\n\
claude-opus-5-5-medium\tClaude Opus 5.5 (Medium)\r\n\
claude-opus-5-5-high\tClaude Opus 5.5 (High)\r\n\
gpt-oss-120b-medium\tGPT-OSS 120B (Medium)\r\n";
        let m = parse_models(real);
        let ids: Vec<&str> = m.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(
            ids,
            ["gemini-4-neon", "gemini-pro-orchestrator-neon", "gemini-pro-orchestrator", "claude-opus-5-5", "gpt-oss-120b-medium"]
        );
        assert_eq!(m[0].efforts, ["low", "medium", "high", "xhigh"]);
        assert_eq!(m[1].name, "Orchestrator (Neon)");
        assert!(m[1].efforts.is_empty());
        assert_eq!(m[3].efforts, ["low", "medium", "high"]);
        assert_eq!((m[4].name.as_str(), m[4].efforts.len()), ("GPT-OSS 120B (Medium)", 0));

        let tty = concat!(
            "\r\u{280b} Fetching available models...\r\u{2819} Fetching available models...",
            "\r\x1b[Kgemini-4-neon-high             Gemini 4 Neon (High)\r\n",
            "gemini-4-neon-xhigh            Gemini 4 Neon (XHigh)\r\n",
            "gpt-oss-120b-medium            GPT-OSS 120B (Medium)\r\n",
        );
        let tty_models = parse_models(tty);
        assert_eq!(tty_models.len(), 2);
        assert_eq!((tty_models[0].id.as_str(), tty_models[0].name.as_str()), ("gemini-4-neon", "Gemini 4 Neon"));
        assert_eq!(tty_models[0].efforts, ["high", "xhigh"]);
        assert_eq!((tty_models[1].id.as_str(), tty_models[1].name.as_str()), ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)"));
    }

    #[test]
    fn sign_in_messages_and_auth_detection_match_location() {
        assert!(is_auth_error("Error: Please sign in to view available models. Launch the CLI without arguments to sign in."));
        assert!(is_auth_error("Print mode: not logged in and no controlling terminal"));
        assert!(!is_auth_error("network timeout"));
        assert_eq!(not_signed_in(&Location::Local), NOT_SIGNED_IN);
        let wsl = not_signed_in(&Location::Wsl { distro: "Ubuntu".into() });
        assert!(wsl.contains("isn't signed in") && wsl.contains("Ubuntu") && wsl.contains("agy acp --login"));
        let ssh = not_signed_in(&Location::Ssh { host: "mac".into() });
        assert!(ssh.contains("isn't signed in") && ssh.contains("mac") && ssh.contains("AGY_CLI_FORCE_FILE_TOKEN_STORAGE=1"));
    }

    #[test]
    fn lists_pre_chat_commands() {
        let names: Vec<String> = commands().into_iter().map(|c| c.name).collect();
        for expected in ["goal", "plan", "compact", "swarm", "teamwork", "boost"] {
            assert!(names.iter().any(|n| n == expected), "missing /{expected} in {names:?}");
        }
    }

    #[test]
    fn discovers_local_skills_with_walkup_and_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        let repo = outside.join("repo");
        let sub = repo.join("packages").join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();

        let write_skill = |root: &Path, dir: &str, body: &str| {
            let p = root.join(dir);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("SKILL.md"), body).unwrap();
        };

        write_skill(&outside.join(".agents/skills"), "leaked", "---\nname: leaked\ndescription: should not load\n---\n");
        write_skill(&sub.join(".agents/skills"), "deploy", "---\nname: deploy\ndescription: \"Workspace deploy\"\n---\n# Deploy\n");
        write_skill(&sub.join(".agent/skills"), "lint", "---\nname: lint\ndescription: >\n  Run project\n  linters.\n---\n");
        write_skill(&repo.join(".agents/skills"), "deploy", "---\nname: deploy\ndescription: Repo deploy\n---\n");
        write_skill(&repo.join(".agents/skills"), "fallback-name", "---\ndescription: 'Uses folder''s name'\n---\n");
        write_skill(&repo.join(".agent/skills"), "broken", "no frontmatter here");
        write_skill(&home.join(".agents/skills"), "rex", "---\nname: rex\ndescription: Terminal multiplexer\n---\n");
        write_skill(&home.join(".gemini/config/skills"), "rex", "---\nname: rex\ndescription: Duplicate global\n---\n");
        write_skill(&home.join(".gemini/config/skills"), "audit", "\u{feff}---\nname: /audit\ndescription: Security audit\n---\n");

        let skills = discover_skills(&sub, &home);
        let pairs: Vec<(&str, &str)> = skills.iter().map(|s| (s.name.as_str(), s.description.as_str())).collect();
        assert_eq!(
            pairs,
            [
                ("deploy", "Workspace deploy"),
                ("lint", "Run project linters."),
                ("fallback-name", "Uses folder's name"),
                ("rex", "Terminal multiplexer"),
                ("audit", "Security audit"),
            ]
        );
        assert!(skills.iter().all(|s| s.skill && s.hint.is_empty()));
    }
}
