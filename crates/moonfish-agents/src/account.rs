//! Who each CLI is signed in as, and its plan limits.
//!
//! - Sign-in: `claude auth status --json` and `codex login status` (run where the project is);
//!   a ChatGPT sign-in's email and plan come from `codex app-server`'s `account/read`.
//! - Claude limits: `GET https://api.anthropic.com/api/oauth/usage` with the CLI's own OAuth
//!   token from macOS Keychain or `~/.claude/.credentials.json`.
//! - Codex limits: `account/read` and `account/rateLimits/read` from a short-lived
//!   `codex app-server`.
//! - Antigravity: see [`crate::antigravity`].

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use moonfish_core::settings::{AgentKind, EnvVar, Location};
use moonfish_transport::Spec;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub signed_in: bool,
    /// Email, or how it's signed in ("API key").
    pub label: Option<String>,
    /// The plan: "max", "pro"…
    pub plan: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    pub label: String,
    pub used_percent: f64,
    /// Unix milliseconds.
    pub resets_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub windows: Vec<LimitWindow>,
    pub error: Option<String>,
    /// Rows that aren't windows: a balance, or why it isn't shown.
    #[serde(default)]
    pub extra: Vec<LimitExtra>,
    /// The agent's CLI isn't installed there: nothing to show for it.
    #[serde(default)]
    pub missing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_plan: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub agent: AgentKind,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub locations: Vec<String>,
    pub limits: Limits,
}

#[derive(Debug, Clone)]
pub struct UsageSample {
    pub agent: AgentKind,
    pub location_label: String,
    pub limits: Limits,
}

pub fn location_label(location: &Location) -> String {
    match location {
        Location::Local => "This computer".into(),
        Location::Ssh { host } => format!("SSH · {host}"),
        Location::Wsl { distro } => format!("WSL · {distro}"),
    }
}

pub fn is_healthy_limits(limits: &Limits) -> bool {
    !limits.missing && limits.error.is_none()
}

/// Picks the best single [`Limits`] from multi-location samples for one agent:
/// first healthy sample (preferring non-empty windows), else first non-missing error, else missing.
pub fn best_limits(samples: &[UsageSample]) -> Limits {
    if let Some(s) = samples.iter().find(|s| is_healthy_limits(&s.limits) && !s.limits.windows.is_empty()) {
        return s.limits.clone();
    }
    if let Some(s) = samples.iter().find(|s| is_healthy_limits(&s.limits)) {
        return s.limits.clone();
    }
    if let Some(s) = samples.iter().find(|s| !s.limits.missing) {
        return s.limits.clone();
    }
    samples.first().map(|s| s.limits.clone()).unwrap_or_else(|| Limits { missing: true, ..Default::default() })
}

/// Groups and deduplicates usage across locations by `(agent, normalized_email)`.
///
/// - Same `(agent, email)` across multiple locations collapses into one entry combining location
///   labels and preferring a healthy non-error [`Limits`].
/// - Distinct emails for the same agent stay separate.
/// - Error and missing rows for an agent are suppressed whenever at least one location returned a
///   healthy [`Limits`] for that agent.
pub fn dedupe_usage_by_account(samples: Vec<UsageSample>) -> Vec<AccountUsage> {
    let mut healthy_agents: Vec<AgentKind> = Vec::new();
    for s in &samples {
        if is_healthy_limits(&s.limits) && !healthy_agents.contains(&s.agent) {
            healthy_agents.push(s.agent);
        }
    }

    struct Group {
        agent: AgentKind,
        norm_email: Option<String>,
        email: Option<String>,
        plan: Option<String>,
        locations: Vec<String>,
        limits: Limits,
    }

    let mut groups: Vec<Group> = Vec::new();
    for sample in samples {
        if sample.limits.missing {
            continue;
        }
        let norm = sample.limits.account_email.as_deref().map(str::trim).filter(|e| !e.is_empty()).map(str::to_lowercase);
        if let Some(ref key) = norm {
            if let Some(g) = groups.iter_mut().find(|g| g.agent == sample.agent && g.norm_email.as_deref() == Some(key.as_str())) {
                if !g.locations.contains(&sample.location_label) {
                    g.locations.push(sample.location_label);
                }
                let old_ok = is_healthy_limits(&g.limits);
                let new_ok = is_healthy_limits(&sample.limits);
                let replace = (!old_ok && new_ok) || (old_ok && new_ok && g.limits.windows.is_empty() && !sample.limits.windows.is_empty());
                if replace {
                    if let Some(e) = sample.limits.account_email.clone() {
                        g.email = Some(e);
                    }
                    if let Some(p) = sample.limits.account_plan.clone() {
                        g.plan = Some(p);
                    }
                    g.limits = sample.limits;
                } else if g.plan.is_none() {
                    g.plan = sample.limits.account_plan.clone();
                }
                continue;
            }
        }
        groups.push(Group {
            agent: sample.agent,
            norm_email: norm,
            email: sample.limits.account_email.clone(),
            plan: sample.limits.account_plan.clone(),
            locations: vec![sample.location_label],
            limits: sample.limits,
        });
    }

    groups
        .into_iter()
        .filter(|g| is_healthy_limits(&g.limits) || !healthy_agents.contains(&g.agent))
        .map(|g| AccountUsage { agent: g.agent, email: g.email, plan: g.plan, locations: g.locations, limits: g.limits })
        .collect()
}

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LimitExtra {
    pub label: String,
    /// `None`: unavailable (see `note`).
    pub value: Option<String>,
    pub note: Option<String>,
}

fn program(kind: AgentKind, binary: &str) -> String {
    if binary.trim().is_empty() {
        kind.default_binary().into()
    } else {
        binary.trim().into()
    }
}

/// Sign-in state from the CLI itself, with the provider's environment (a config folder or key
/// set in Settings → Providers decides which account it is).
pub fn account(kind: AgentKind, binary: &str, location: &Location, env: &[EnvVar]) -> Account {
    let prog = program(kind, binary);
    let args: Vec<String> = match kind {
        AgentKind::Claude => vec!["auth".into(), "status".into(), "--json".into()],
        AgentKind::Codex => vec!["login".into(), "status".into()],
        // `agy` has no status command: its token store says who.
        AgentKind::Antigravity => return crate::antigravity::account(location, env),
    };
    let cwd =
        if matches!(location, Location::Local) { moonfish_core::paths::home_dir().to_string_lossy().into_owned() } else { "~".into() };
    let out = match moonfish_transport::run(&Spec { location, cwd: &cwd, program: &prog, args, env }, None, Duration::from_secs(20)) {
        Ok(o) => o,
        Err(e) => return Account { error: Some(e), ..Default::default() },
    };
    match kind {
        AgentKind::Claude => parse_claude_status(&out.stdout).unwrap_or_else(|| Account {
            error: Some(first_line(&out.stderr).unwrap_or("couldn't read `claude auth status`").into()),
            ..Default::default()
        }),
        AgentKind::Codex => {
            let mut a = parse_codex_status(&format!("{}\n{}", out.stdout, out.stderr));
            // `login status` only says "ChatGPT"; the app-server knows which account.
            if a.signed_in && a.label.as_deref() == Some("ChatGPT") {
                if let Ok(v) = codex_once(&prog, location, env, "account/read", json!({})) {
                    apply_codex_account(&mut a, &v);
                }
            }
            a
        }
        AgentKind::Antigravity => unreachable!("answered above"),
    }
}

fn first_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).find(|l| !l.is_empty())
}

pub fn parse_claude_status(stdout: &str) -> Option<Account> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    let signed_in = v["loggedIn"].as_bool().unwrap_or(false);
    let label = v["email"].as_str().map(String::from).or_else(|| match v["authMethod"].as_str() {
        Some("api_key") | Some("apiKey") => Some("API key".into()),
        _ => None,
    });
    Some(Account { signed_in, label, plan: v["subscriptionType"].as_str().map(String::from), error: None })
}

pub fn parse_codex_status(text: &str) -> Account {
    let line = first_line(text).unwrap_or("");
    let lower = line.to_lowercase();
    if lower.starts_with("logged in") {
        let how = line.split(" using ").nth(1).map(|s| s.trim().to_string());
        Account { signed_in: true, label: how, plan: None, error: None }
    } else {
        Account {
            signed_in: false,
            label: None,
            plan: None,
            error: (!line.is_empty() && !lower.contains("not logged in")).then(|| line.to_string()),
        }
    }
}

/// `account/read`'s ChatGPT email and plan, over `login status`'s "ChatGPT".
pub fn apply_codex_account(a: &mut Account, v: &Value) {
    let acct = &v["account"];
    if let Some(email) = acct["email"].as_str().filter(|e| e.contains('@')) {
        a.label = Some(email.to_string());
    }
    if let Some(plan) = acct["planType"].as_str() {
        a.plan = Some(plan.to_string());
    }
}

// ---- Claude ----------------------------------------------------------------------------------

/// Claude's plan limits for the sign-in on `location` (this computer's credentials file, or the
/// one in a WSL distro or on an SSH host).
pub fn claude_limits(location: &Location, env: &[EnvVar]) -> Limits {
    let token = claude_usage_token(location, env);
    let Some(token) = token else {
        return Limits {
            error: Some(
                "Claude plan usage is unavailable because Moonfish found no readable Claude plan credential for this location.".into(),
            ),
            ..Default::default()
        };
    };
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().new_agent();
    // Claude may rotate the on-disk token while a usage request is in flight. Retry only if
    // it actually changed; never refresh or overwrite the CLI's credentials ourselves.
    let mut token = token;
    for attempt in 0..2 {
        let resp = agent
            .get("https://api.anthropic.com/api/oauth/usage")
            .header("Authorization", &format!("Bearer {token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", "claude-code/2.1.0")
            .call();
        return match resp {
            Ok(mut r) => match r.body_mut().read_json::<Value>() {
                Ok(v) => Limits { windows: parse_claude_usage(&v), ..Default::default() },
                Err(_) => Limits { error: Some("Claude's usage endpoint returned an unexpected response.".into()), ..Default::default() },
            },
            Err(ureq::Error::StatusCode(status)) => {
                if status == 401 && attempt == 0 {
                    if let Some(next) = claude_usage_token(location, env).filter(|next| next != &token) {
                        token = next;
                        continue;
                    }
                }
                Limits { error: Some(claude_usage_error(status)), ..Default::default() }
            }
            Err(e) => Limits { error: Some(format!("couldn't load Claude limits: {e}")), ..Default::default() },
        };
    }
    unreachable!("a second usage attempt always returns")
}

fn claude_usage_error(status: u16) -> String {
    match status {
        401 | 403 => {
            format!("Claude's usage endpoint rejected access with HTTP {status}. Chat sign-in is checked separately.")
        }
        _ => format!("Claude's usage request failed with HTTP {status}."),
    }
}

/// Match the provider environment without changing the desktop process's environment.
fn local_env(env: &[EnvVar], key: &str) -> Option<String> {
    env.iter().rev().find(|v| v.key == key).map(|v| v.value.clone()).or_else(|| std::env::var(key).ok())
}

fn credential_home(env: &[EnvVar]) -> std::path::PathBuf {
    local_env(env, "HOME").filter(|s| !s.is_empty()).map(Into::into).unwrap_or_else(moonfish_core::paths::home_dir)
}

#[rustfmt::skip]
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// First 8 lowercase hex characters of `SHA-256(input)` (matches Claude Code's Keychain suffix).
fn sha256_prefix8(input: &[u8]) -> String {
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut msg = input.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, &b) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(b);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for (&k, &wi) in SHA256_K.iter().zip(&w) {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(k).wrapping_add(wi);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (dst, add) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *dst = dst.wrapping_add(add);
        }
    }
    format!("{:08x}", h[0])
}

/// The macOS Keychain service name Claude Code uses for `env`.
pub fn claude_keychain_service(env: &[EnvVar]) -> String {
    let dir = local_env(env, "CLAUDE_SECURESTORAGE_CONFIG_DIR")
        .filter(|s| !s.is_empty())
        .or_else(|| local_env(env, "CLAUDE_CONFIG_DIR").filter(|s| !s.is_empty()));
    match dir {
        Some(d) => format!("Claude Code-credentials-{}", sha256_prefix8(d.as_bytes())),
        None => "Claude Code-credentials".into(),
    }
}

/// Extracts Claude Code's OAuth access token from `.credentials.json` or macOS Keychain JSON.
pub fn parse_claude_credentials(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text.trim())
        .ok()
        .and_then(|v| v["claudeAiOauth"]["accessToken"].as_str().map(String::from))
        .filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
fn macos_keychain_claude_credentials(env: &[EnvVar]) -> Option<String> {
    let service = claude_keychain_service(env);
    let user = local_env(env, "USER").filter(|s| !s.is_empty());
    let run = |args: &[&str]| {
        std::process::Command::new("/usr/bin/security")
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .filter(|s| !s.trim().is_empty())
    };
    if let Some(account) = user {
        if let Some(out) = run(&["find-generic-password", "-s", &service, "-a", &account, "-w"]) {
            return Some(out);
        }
    }
    run(&["find-generic-password", "-s", &service, "-w"])
}

const REMOTE_CLAUDE_CREDENTIALS: &str = r#"f="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json"
if [ -s "$f" ]; then
  cat "$f" 2>/dev/null
  exit 0
fi
if command -v security >/dev/null 2>&1; then
  d="${CLAUDE_SECURESTORAGE_CONFIG_DIR:-$CLAUDE_CONFIG_DIR}"
  if [ -n "$d" ]; then
    h=$(printf '%s' "$d" | shasum -a 256 | cut -c1-8)
    svc="Claude Code-credentials-$h"
  else
    svc="Claude Code-credentials"
  fi
  u="${USER:-$(id -un)}"
  security find-generic-password -s "$svc" -a "$u" -w 2>/dev/null || security find-generic-password -s "$svc" -w 2>/dev/null
fi"#;

fn claude_usage_token(location: &Location, env: &[EnvVar]) -> Option<String> {
    match location {
        Location::Local => {
            #[cfg(target_os = "macos")]
            if let Some(token) = macos_keychain_claude_credentials(env).and_then(|t| parse_claude_credentials(&t)) {
                return Some(token);
            }
            let dir = local_env(env, "CLAUDE_CONFIG_DIR")
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| credential_home(env).join(".claude"));
            std::fs::read_to_string(dir.join(".credentials.json")).ok().and_then(|t| parse_claude_credentials(&t))
        }
        _ => {
            let args = vec!["-c".into(), REMOTE_CLAUDE_CREDENTIALS.into()];
            moonfish_transport::run(&Spec { location, cwd: "~", program: "sh", args, env }, None, Duration::from_secs(20))
                .ok()
                .filter(|o| o.ok())
                .and_then(|o| parse_claude_credentials(&o.stdout))
        }
    }
}

pub fn parse_claude_usage(v: &Value) -> Vec<LimitWindow> {
    let mut out = Vec::new();
    let mut push = |label: &str, w: &Value| {
        let pct = w["utilization"].as_f64().or(w["used_percentage"].as_f64());
        if let Some(p) = pct {
            out.push(LimitWindow { label: label.into(), used_percent: p.clamp(0.0, 100.0), resets_at: reset_ms(&w["resets_at"]) });
        }
    };
    push("Current session (5 hours)", &v["five_hour"]);
    push("This week, all models", &v["seven_day"]);
    for (k, label) in
        [("seven_day_opus", "This week, Opus"), ("seven_day_sonnet", "This week, Sonnet"), ("seven_day_fable", "This week, Fable")]
    {
        push(label, &v[k]);
    }
    for l in v["limits"].as_array().into_iter().flatten() {
        if l["kind"] == "weekly_scoped" {
            if let Some(name) = l["scope"]["model"]["display_name"].as_str() {
                let label = format!("This week, {name}");
                if !out.iter().any(|w| w.label == label) {
                    if let Some(p) = l["percent"].as_f64() {
                        out.push(LimitWindow { label, used_percent: p.clamp(0.0, 100.0), resets_at: reset_ms(&l["resets_at"]) });
                    }
                }
            }
        }
    }
    out
}

/// Seconds, milliseconds or an ISO date → Unix milliseconds.
pub(crate) fn reset_ms(v: &Value) -> Option<u64> {
    let n = v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()));
    if let Some(n) = n {
        return Some(if n > 1e10 { n as u64 } else { (n * 1000.0) as u64 });
    }
    v.as_str().and_then(iso_ms)
}

/// `2026-11-07T05:00:00.123+00:00` / `…Z` → Unix ms (UTC offsets only, which is what the API sends).
fn iso_ms(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z');
    let (time, offset) = match time.rfind(['+', '-']) {
        Some(i) if i > 0 => (&time[..i], &time[i..]),
        _ => (time, "+00:00"),
    };
    let mut t = time.split(':');
    let (hh, mm) = (t.next()?.parse::<i64>().ok()?, t.next()?.parse::<i64>().ok()?);
    let ss = t.next().and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let mut o = offset[1..].split(':');
    let off =
        sign * (o.next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0) * 60 + o.next().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0));
    // Days from the civil date (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + hh * 3600 + mm * 60 - off * 60;
    Some((secs as f64 * 1000.0 + ss * 1000.0) as u64)
}

// ---- Codex -----------------------------------------------------------------------------------

pub fn codex_limits(binary: &str, location: &Location, env: &[EnvVar]) -> Limits {
    let prog = program(AgentKind::Codex, binary);
    let home =
        if matches!(location, Location::Local) { moonfish_core::paths::home_dir().to_string_lossy().into_owned() } else { "~".into() };
    match with_codex(&prog, location, &home, env, |call| {
        let rate = call("account/rateLimits/read", Value::Null)?;
        let acct = call("account/read", json!({})).ok();
        Ok((rate, acct))
    }) {
        Ok((v, acct)) => {
            let mut limits = Limits { windows: parse_codex_limits(&v), ..Default::default() };
            if let Some(a) = acct {
                let mut info = Account { signed_in: true, ..Default::default() };
                apply_codex_account(&mut info, &a);
                limits.account_email = info.label.filter(|l| l.contains('@'));
                limits.account_plan = info.plan;
            }
            limits
        }
        Err(e) => Limits { error: Some(e), ..Default::default() },
    }
}

/// One request to a short-lived `codex app-server` (where `location` says).
pub fn codex_once(prog: &str, location: &Location, env: &[EnvVar], method: &str, params: Value) -> Result<Value, String> {
    let home =
        if matches!(location, Location::Local) { moonfish_core::paths::home_dir().to_string_lossy().into_owned() } else { "~".into() };
    with_codex(prog, location, &home, env, |call| call(method, params))
}

pub fn with_codex<T>(
    prog: &str,
    location: &Location,
    cwd: &str,
    env: &[EnvVar],
    run: impl FnOnce(&mut dyn FnMut(&str, Value) -> Result<Value, String>) -> Result<T, String>,
) -> Result<T, String> {
    let mut child = moonfish_transport::spawn(&Spec { location, cwd, program: prog, args: vec!["app-server".into()], env })?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if v.get("method").is_none() && v["id"].is_u64() && tx.send(v).is_err() {
                    break;
                }
            }
        }
    });
    let mut send = |v: Value| writeln!(stdin, "{v}").map_err(|e| e.to_string());
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "moonfish", "title": "Moonfish", "version": env!("CARGO_PKG_VERSION")}, "capabilities": null}}),
    )?;
    let result = (|| {
        rx.recv_timeout(Duration::from_secs(20)).map_err(|_| "Codex didn't start".to_string())?;
        send(json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        let mut id = 1;
        run(&mut |method, params| {
            id += 1;
            send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
            loop {
                let v = rx.recv_timeout(Duration::from_secs(20)).map_err(|_| format!("Codex didn't answer {method}"))?;
                if v["id"] == id {
                    return match v.get("error") {
                        Some(e) => Err(e["message"].as_str().unwrap_or("Codex couldn't answer").to_string()),
                        None => Ok(v["result"].clone()),
                    };
                }
            }
        })
    })();
    moonfish_transport::stop_tree(&mut child, Duration::ZERO);
    result
}

pub fn parse_codex_limits(v: &Value) -> Vec<LimitWindow> {
    let rl = &v["rateLimits"];
    let mut out = Vec::new();
    for key in ["primary", "secondary"] {
        let w = &rl[key];
        if let Some(p) = w["usedPercent"].as_f64() {
            let mins = w["windowDurationMins"].as_u64().unwrap_or(0);
            let label = match mins {
                300 => "5 hour window".to_string(),
                10080 => "Weekly window".to_string(),
                m if m >= 1440 => format!("{} day window", m / 1440),
                m if m > 0 => format!("{} hour window", (m as f64 / 60.0).round()),
                _ => "Window".to_string(),
            };
            out.push(LimitWindow { label, used_percent: p.clamp(0.0, 100.0), resets_at: reset_ms(&w["resetsAt"]) });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sign_in() {
        let a = parse_claude_status(r#"{"loggedIn":true,"authMethod":"claude.ai","email":"me@x.dev","subscriptionType":"max"}"#).unwrap();
        assert!(a.signed_in);
        assert_eq!(a.label.as_deref(), Some("me@x.dev"));
        assert_eq!(a.plan.as_deref(), Some("max"));
        let c = parse_codex_status("Logged in using ChatGPT\n");
        assert!(c.signed_in && c.label.as_deref() == Some("ChatGPT"));
        let mut c = c;
        apply_codex_account(&mut c, &json!({"account":{"type":"chatgpt","email":"me@x.dev","planType":"pro"}}));
        assert_eq!((c.label.as_deref(), c.plan.as_deref()), (Some("me@x.dev"), Some("pro")));
        let mut k = parse_codex_status("Logged in using an API key\n");
        apply_codex_account(&mut k, &json!({"account":{"type":"apiKey"}}));
        assert_eq!(k.label.as_deref(), Some("an API key"));
        assert!(!parse_codex_status("Not logged in").signed_in);
    }

    #[test]
    fn usage_errors_do_not_infer_chat_sign_in() {
        for status in [401, 403] {
            let claude = claude_usage_error(status);
            assert!(claude.contains("usage endpoint"));
            assert!(!claude.contains("expired"));
        }
    }

    #[test]
    fn usage_reads_the_configured_provider_credentials() {
        let dir = std::env::temp_dir().join(format!("moonfish-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".credentials.json"), r#"{"claudeAiOauth":{"accessToken":"test-claude"}}"#).unwrap();
        let var = |key: &str, value: &str| EnvVar { key: key.into(), value: value.into(), in_keychain: false };
        let path = dir.to_str().unwrap();
        assert_eq!(claude_usage_token(&Location::Local, &[var("CLAUDE_CONFIG_DIR", path)]).as_deref(), Some("test-claude"));
        assert_eq!(credential_home(&[var("HOME", path)]), dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_limits() {
        let w = parse_claude_usage(
            &json!({"five_hour":{"utilization":12.0,"resets_at":"2026-11-07T05:00:00+00:00"},"seven_day":{"utilization":47.0,"resets_at":1794301200}}),
        );
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].resets_at, Some(1_794_027_600_000));
        assert_eq!(w[1].resets_at, Some(1_794_301_200_000));
        let c = parse_codex_limits(
            &json!({"rateLimits":{"primary":{"usedPercent":45,"windowDurationMins":10080,"resetsAt":1794410242},"secondary":null}}),
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].label, "Weekly window");
    }

    #[test]
    fn dedupes_usage_by_account_and_suppresses_errors_when_healthy() {
        let win = |pct| vec![LimitWindow { label: "5h".into(), used_percent: pct, resets_at: None }];
        let healthy = |email: &str, plan: &str, pct| Limits {
            windows: win(pct),
            account_email: Some(email.into()),
            account_plan: Some(plan.into()),
            ..Default::default()
        };
        let err = |em: Option<&str>, e: &str| Limits { error: Some(e.into()), account_email: em.map(String::from), ..Default::default() };

        // Same email across Local and SSH collapses into one row with both locations.
        let collapsed = dedupe_usage_by_account(vec![
            UsageSample { agent: AgentKind::Claude, location_label: "This computer".into(), limits: healthy("me@x.dev", "max", 20.0) },
            UsageSample { agent: AgentKind::Claude, location_label: "SSH · mac".into(), limits: healthy("ME@X.DEV", "max", 20.0) },
        ]);
        assert_eq!(collapsed.len(), 1);
        assert_eq!(collapsed[0].locations, ["This computer", "SSH · mac"]);
        assert_eq!(collapsed[0].email.as_deref(), Some("me@x.dev"));

        // Different emails stay separate.
        let split = dedupe_usage_by_account(vec![
            UsageSample { agent: AgentKind::Codex, location_label: "This computer".into(), limits: healthy("work@co.com", "pro", 10.0) },
            UsageSample { agent: AgentKind::Codex, location_label: "SSH · mac".into(), limits: healthy("home@gmail.com", "plus", 40.0) },
        ]);
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].email.as_deref(), Some("work@co.com"));
        assert_eq!(split[1].email.as_deref(), Some("home@gmail.com"));

        // Healthy remote location suppresses local unauthenticated error and missing rows.
        let samples = vec![
            UsageSample { agent: AgentKind::Antigravity, location_label: "This computer".into(), limits: err(None, "not signed in") },
            UsageSample {
                agent: AgentKind::Antigravity,
                location_label: "WSL · Ubuntu".into(),
                limits: Limits { missing: true, ..Default::default() },
            },
            UsageSample {
                agent: AgentKind::Antigravity,
                location_label: "SSH · mac".into(),
                limits: healthy("dev@gmail.com", "ultra", 15.0),
            },
        ];
        assert_eq!(best_limits(&samples).account_email.as_deref(), Some("dev@gmail.com"));
        let grouped = dedupe_usage_by_account(samples);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].locations, ["SSH · mac"]);
        assert_eq!(grouped[0].email.as_deref(), Some("dev@gmail.com"));

        // Same email where local errored and remote is healthy keeps combined locations + healthy limits.
        let recovered = dedupe_usage_by_account(vec![
            UsageSample {
                agent: AgentKind::Claude,
                location_label: "This computer".into(),
                limits: err(Some("me@x.dev"), "token unreadable"),
            },
            UsageSample { agent: AgentKind::Claude, location_label: "SSH · mac".into(), limits: healthy("me@x.dev", "max", 30.0) },
        ]);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].locations, ["This computer", "SSH · mac"]);
        assert!(recovered[0].limits.error.is_none());
        assert_eq!(recovered[0].limits.windows[0].used_percent, 30.0);

        assert_eq!(location_label(&Location::Local), "This computer");
        assert_eq!(location_label(&Location::Ssh { host: "mac".into() }), "SSH · mac");
        assert_eq!(location_label(&Location::Wsl { distro: "Ubuntu".into() }), "WSL · Ubuntu");
    }

    #[test]
    fn builds_claude_keychain_service_name_and_parses_credentials() {
        let var = |key: &str, value: &str| EnvVar { key: key.into(), value: value.into(), in_keychain: false };
        assert_eq!(
            claude_keychain_service(&[var("CLAUDE_SECURESTORAGE_CONFIG_DIR", ""), var("CLAUDE_CONFIG_DIR", "")]),
            "Claude Code-credentials"
        );
        assert_eq!(
            claude_keychain_service(&[var("CLAUDE_SECURESTORAGE_CONFIG_DIR", ""), var("CLAUDE_CONFIG_DIR", "/Users/me/.claude-work")]),
            "Claude Code-credentials-1e91dd84"
        );
        assert_eq!(
            claude_keychain_service(&[
                var("CLAUDE_CONFIG_DIR", "/Users/me/.claude-work"),
                var("CLAUDE_SECURESTORAGE_CONFIG_DIR", "/Users/developer/Library/Application Support/custom-claude-config-dir"),
            ]),
            "Claude Code-credentials-85137797"
        );
        assert_eq!(sha256_prefix8(b""), "e3b0c442");
        assert_eq!(sha256_prefix8(&[b'a'; 128]), "6836cf13");

        assert_eq!(parse_claude_credentials(r#"  {"claudeAiOauth":{"accessToken":"tok-123"}}  "#).as_deref(), Some("tok-123"));
        assert_eq!(parse_claude_credentials(r#"{"claudeAiOauth":{"accessToken":""}}"#), None);
        assert_eq!(parse_claude_credentials(r#"{"other":true}"#), None);
        assert_eq!(parse_claude_credentials("not json"), None);
    }
}
