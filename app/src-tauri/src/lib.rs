//! The Tauri shell: the window and the commands the UI calls. The work lives in the `moonfish-*`
//! crates; this file only adapts them to IPC.

mod attention;
mod chats;
mod git;
mod remote;
mod remote_firewall;
mod secrets;

use std::sync::{Arc, Mutex};

use moonfish_agents::account::{self, Account, Limits};
use moonfish_agents::driver::Prompt;
use moonfish_agents::event::Decision;
use moonfish_agents::login::{LoginEvent, LoginManager, LoginMethod, LoginStart};
use moonfish_agents::probe::{self, Probe};
use moonfish_core::chats::{ChatMeta, ChatStore};
use moonfish_core::settings::{AgentKind, Location, PermissionMode, Project, Settings};
use moonfish_core::{paths, ssh};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// The settings as last saved, shared by every command.
pub(crate) struct AppState {
    pub(crate) settings: Mutex<Settings>,
    pub(crate) chats: chats::Chats,
    remote: remote::Remote,
    logins: LoginManager,
}

#[cfg(all(test, target_os = "linux"))]
impl AppState {
    /// The app's state over `settings` and a chats folder, with nothing else running.
    pub(crate) fn for_tests(settings: Settings, store: ChatStore) -> AppState {
        AppState {
            settings: Mutex::new(settings),
            chats: chats::Chats::new(store),
            remote: remote::Remote::default(),
            logins: LoginManager::default(),
        }
    }
}

pub(crate) type CmdResult<T> = Result<T, String>;

pub(crate) fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: &'static str,
    platform: &'static str,
    data_dir: String,
    home: String,
    /// How this copy was installed (`nsis`, `deb`, `appimage`…); none in development.
    bundle: Option<&'static str>,
    /// The settings file couldn't be read at start, and what was done with it.
    settings_problem: Option<String>,
}

/// Set once at start when settings.json couldn't be read.
static SETTINGS_PROBLEM: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn bundle_name() -> Option<&'static str> {
    use tauri::utils::config::BundleType;
    Some(match tauri::utils::platform::bundle_type()? {
        BundleType::AppImage => "appimage",
        BundleType::Deb => "deb",
        BundleType::Rpm => "rpm",
        BundleType::Nsis => "nsis",
        BundleType::Msi => "msi",
        _ => return None,
    })
}

/// Runs `f` on a worker thread. Tauri runs plain (non-async) commands on the main thread, so
/// one that waits on the disk, the keychain or an agent would freeze the window meanwhile.
async fn off_main<T: Send + 'static>(app: AppHandle, f: impl FnOnce(&AppHandle, &AppState) -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        f(&app, &state)
    })
    .await
    .map_err(err)
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        platform: if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        },
        data_dir: paths::app_dir().to_string_lossy().into_owned(),
        home: paths::home_dir().to_string_lossy().into_owned(),
        bundle: bundle_name(),
        settings_problem: SETTINGS_PROBLEM.get().cloned(),
    }
}

#[tauri::command]
fn settings_get(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

/// Replaces the settings with `settings` and saves them.
#[tauri::command]
async fn settings_set(app: AppHandle, mut settings: Settings) -> CmdResult<Settings> {
    off_main(app, move |app, state| {
        let before = state.settings.lock().unwrap().clone();
        // Phones: on/off, port and paired devices change only through the remote_* commands.
        settings.remote = before.remote.clone();
        secrets::stash(&mut settings, &before)?;
        settings.save(&paths::settings_file()).map_err(err)?;
        *state.settings.lock().unwrap() = settings.clone();
        state.chats.changed(app);
        Ok(settings)
    })
    .await?
}

#[tauri::command]
async fn project_add(app: AppHandle, location: Location, path: String) -> CmdResult<(Settings, Project)> {
    off_main(app, move |app, state| {
        let mut s = state.settings.lock().unwrap();
        let mut next = s.clone();
        let project = next.add_project(location, &path).map_err(err)?;
        next.save(&paths::settings_file()).map_err(err)?;
        *s = next.clone();
        drop(s);
        // The folder was a project before and was removed: its chats come back with it.
        state.chats.adopt(&next, &project);
        state.chats.changed(app);
        Ok((next, project))
    })
    .await?
}

#[tauri::command]
async fn project_remove(app: AppHandle, id: String) -> CmdResult<Settings> {
    off_main(app, move |app, state| {
        // Its agents stop (nothing would show them); its chats stay on disk, and come back if the
        // folder is added again.
        for chat in state.chats.store.list().into_iter().filter(|c| c.project_id == id) {
            state.chats.close(&chat.id);
        }
        let mut s = state.settings.lock().unwrap();
        let mut next = s.clone();
        next.remove_project(&id);
        next.save(&paths::settings_file()).map_err(err)?;
        *s = next.clone();
        drop(s);
        state.chats.changed(app);
        Ok(next)
    })
    .await?
}

/// Hosts named in `~/.ssh/config`.
#[tauri::command]
fn ssh_config_hosts() -> Vec<String> {
    ssh::user_config_hosts()
}

#[tauri::command]
async fn ssh_config_read() -> CmdResult<Option<String>> {
    tauri::async_runtime::spawn_blocking(ssh::read_user_config).await.map_err(err)?.map_err(err)
}

#[tauri::command]
async fn ssh_config_save(original: Option<String>, text: String) -> CmdResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        ssh::save_user_config(original.as_deref(), &text).map_err(err)?;
        Ok(ssh::user_config_hosts())
    })
    .await
    .map_err(err)?
}

/// Reaches an SSH host or a WSL distro and lists the tools Moonfish uses there.
#[tauri::command]
async fn location_check(location: Location) -> CmdResult<moonfish_transport::remote::HostCheck> {
    tauri::async_runtime::spawn_blocking(move || moonfish_transport::remote::check(&location)).await.map_err(err)
}

/// A folder on an SSH host or in a WSL distro, and its subfolders (empty path: home).
#[tauri::command]
async fn location_list_dir(location: Location, path: String) -> CmdResult<moonfish_transport::remote::Listing> {
    tauri::async_runtime::spawn_blocking(move || moonfish_transport::remote::list_dir(&location, &path)).await.map_err(err)?
}

/// Writes bytes to a path the user picked in a Save dialog (an image from the viewer).
#[tauri::command]
fn save_bytes(path: String, data: Vec<u8>) -> CmdResult<()> {
    std::fs::write(&path, data).map_err(|e| format!("couldn't write {path}: {e}"))
}

/// The WSL distros on this computer (none when WSL isn't installed, or off Windows).
#[tauri::command]
async fn wsl_distros() -> Vec<moonfish_transport::wsl::Distro> {
    tauri::async_runtime::spawn_blocking(|| if cfg!(windows) { moonfish_transport::wsl::distros().unwrap_or_default() } else { vec![] })
        .await
        .unwrap_or_default()
}

/// Starts a stopped distro (a few seconds the first time).
#[tauri::command]
async fn wsl_start(distro: String) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || moonfish_transport::wsl::start(&distro)).await.map_err(err)?
}

#[derive(serde::Serialize)]
struct WslPath {
    distro: String,
    path: String,
}

/// `\\wsl.localhost\<distro>\…` (from the Windows folder picker) as a distro and a Linux path.
#[tauri::command]
fn wsl_path(path: String) -> Option<WslPath> {
    moonfish_core::wsl::from_unc(&path).map(|(distro, path)| WslPath { distro, path })
}

/// Whether an agent CLI is installed, where, and which version. Runs off the UI thread.
#[tauri::command]
async fn provider_probe(kind: AgentKind, binary: String) -> Probe {
    tauri::async_runtime::spawn_blocking(move || probe::probe(kind, &binary)).await.unwrap_or_else(|e| Probe {
        found: false,
        path: None,
        version: None,
        error: Some(e.to_string()),
        outdated: None,
    })
}

/// Who the agent's CLI is signed in as (on this computer, an SSH host or a WSL distro).
#[tauri::command]
async fn provider_account(state: State<'_, AppState>, kind: AgentKind, location: Option<Location>) -> CmdResult<Account> {
    let location = location.unwrap_or_default();
    let provider = state.settings.lock().unwrap().providers.get(kind).clone();
    let binary = provider.binary_on(&location);
    tauri::async_runtime::spawn_blocking(move || {
        // Its own environment: a config folder or key set in Providers decides the account.
        let env = secrets::resolve(&secrets::scope(kind), &provider.env);
        account::account(kind, &binary, &location, &env)
    })
    .await
    .map_err(err)
}

/// The agent's plan limits for the sign-in on this computer, or in a WSL distro (which is never
/// started just for this).
#[tauri::command]
async fn provider_limits(state: State<'_, AppState>, kind: AgentKind, location: Option<Location>) -> CmdResult<Limits> {
    let location = location.unwrap_or_default();
    let provider = state.settings.lock().unwrap().providers.get(kind).clone();
    tauri::async_runtime::spawn_blocking(move || plan_limits(kind, &provider, &location)).await.map_err(err)
}

fn plan_limits(kind: AgentKind, provider: &moonfish_core::settings::Provider, location: &Location) -> Limits {
    let binary = provider.binary_on(location);
    if let Location::Wsl { distro } = location {
        if !moonfish_transport::wsl::is_running(distro) {
            return account::Limits {
                error: Some(format!("{distro} isn't running. Open one of its projects to see these.")),
                ..Default::default()
            };
        }
    }
    // An agent that isn't installed there has no limits to read (its sign-in may linger
    // after an uninstall). Unknown (unreachable) still tries, and says why it failed.
    let program = if binary.trim().is_empty() { kind.default_binary().to_string() } else { binary.trim().to_string() };
    let installed = match location {
        Location::Local => Some(probe::resolve(kind, &binary).is_some()),
        remote => moonfish_transport::remote::installed(remote, &program),
    };
    if installed == Some(false) {
        return account::Limits { missing: true, ..Default::default() };
    }
    let env = secrets::resolve(&secrets::scope(kind), &provider.env);
    let mut limits = match kind {
        AgentKind::Claude => account::claude_limits(location, &env),
        AgentKind::Codex => account::codex_limits(&binary, location, &env),
        AgentKind::Antigravity => moonfish_agents::antigravity::limits(&binary, location, &env),
    };
    if !limits.missing && limits.account_email.is_none() {
        let acct = account::account(kind, &binary, location, &env);
        if acct.signed_in {
            limits.account_email = acct.label.filter(|l| l.contains('@'));
            if limits.account_plan.is_none() {
                limits.account_plan = acct.plan;
            }
        }
    }
    limits
}

#[derive(serde::Serialize)]
pub(crate) struct Catalog {
    pub(crate) models: Vec<moonfish_agents::event::ModelOption>,
    pub(crate) commands: Vec<moonfish_agents::event::SlashCommand>,
}

/// The models and slash commands an agent offers, for a chat that hasn't started (run where
/// `project_id` is). A chat's own agent keeps both up to date once it runs.
#[tauri::command]
async fn provider_catalog(state: State<'_, AppState>, kind: AgentKind, project_id: Option<String>) -> CmdResult<Catalog> {
    let settings = state.settings.lock().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || catalog(&settings, kind, project_id.as_deref())).await.map_err(err)?
}

/// What [`provider_catalog`] returns; blocking (it runs the CLI).
pub(crate) fn catalog(s: &Settings, kind: AgentKind, project_id: Option<&str>) -> CmdResult<Catalog> {
    let p = project_id.and_then(|id| s.projects.iter().find(|p| p.id == id).cloned());
    let location = p.as_ref().map(|p| p.location.clone()).unwrap_or(Location::Local);
    let cwd = p.map(|p| p.path).unwrap_or_else(|| paths::home_dir().to_string_lossy().into_owned());
    let binary = s.providers.get(kind).binary_on(&location);
    let env = secrets::resolve(&secrets::scope(kind), &s.providers.get(kind).env);
    match kind {
        AgentKind::Claude => {
            moonfish_agents::claude::catalog(&binary, &location, &cwd, &env).map(|(models, commands)| Catalog { models, commands })
        }
        AgentKind::Codex => {
            let prog = if binary.trim().is_empty() { "codex".to_string() } else { binary };
            account::with_codex(&prog, &location, &cwd, &env, |call| moonfish_agents::codex::catalog(call, &cwd))
                .map(|(models, commands)| Catalog { models, commands })
        }
        AgentKind::Antigravity => {
            let opts = moonfish_agents::driver::StartOptions {
                kind,
                cwd,
                location,
                binary,
                model: None,
                effort: None,
                fast_mode: None,
                permission: PermissionMode::Ask,
                resume: None,
                env,
            };
            moonfish_agents::antigravity::catalog(&opts).map(|(models, commands)| Catalog { models, commands })
        }
    }
}

/// Starts an interactive provider sign-in session on this computer, WSL, or SSH.
#[tauri::command]
async fn provider_login_start(
    app: AppHandle,
    agent: AgentKind,
    location: Option<Location>,
    method: Option<LoginMethod>,
) -> CmdResult<LoginStart> {
    let location = location.unwrap_or_default();
    let method = method.unwrap_or_default();
    off_main(app, move |app, state| {
        let provider = state.settings.lock().unwrap().providers.get(agent).clone();
        let binary = provider.binary_on(&location);
        let env = secrets::resolve(&secrets::scope(agent), &provider.env);
        let emit_app = app.clone();
        let login_location = location.clone();
        let sink: moonfish_agents::login::LoginSink = Arc::new(move |event| {
            if matches!(event, LoginEvent::Done { .. }) {
                let state = emit_app.state::<AppState>();
                let settings = state.settings.lock().unwrap().clone();
                state.chats.restart_for_provider(&settings, agent, &login_location);
            }
            let _ = emit_app.emit("login-event", event);
        });
        state.logins.start(agent, &binary, location, env, method, sink)
    })
    .await?
}

/// Submits an authorization code or callback URL to an active provider sign-in session.
#[tauri::command]
async fn provider_login_submit(app: AppHandle, session_id: String, input: String) -> CmdResult<()> {
    off_main(app, move |_, state| state.logins.submit(&session_id, &input)).await?
}

/// Cancels an active provider sign-in session and stops its CLI process.
#[tauri::command]
async fn provider_login_cancel(app: AppHandle, session_id: String) -> CmdResult<()> {
    off_main(app, move |_, state| state.logins.cancel(&session_id)).await?
}

// ---- Chats -----------------------------------------------------------------------------------

#[tauri::command]
async fn chat_list(app: AppHandle) -> Vec<ChatMeta> {
    off_main(app, move |_, state| state.chats.store.list()).await.unwrap_or_default()
}

/// What each chat waits on the user for (`approval`, `plan` or `question`), whether or not the
/// window has it open: a phone may have started it.
#[tauri::command]
async fn chat_waiting(app: AppHandle) -> std::collections::BTreeMap<String, &'static str> {
    off_main(app, move |_, state| state.chats.waiting()).await.unwrap_or_default()
}

/// Chats with background work running (commands, subagents), whether or not the window has
/// them open.
/// Chats mid-turn, whether or not the window has them open (a phone may drive them).
#[tauri::command]
async fn chat_busy(app: AppHandle) -> Vec<String> {
    off_main(app, |_, state| state.chats.busy()).await.unwrap_or_default()
}

/// Stops every agent, keeping what they'd streamed (before an update restarts the app).
#[tauri::command]
async fn chats_stop_all(app: AppHandle) -> CmdResult<()> {
    off_main(app, |_, state| state.chats.stop_all()).await
}

#[tauri::command]
async fn chat_background(app: AppHandle) -> Vec<String> {
    off_main(app, move |_, state| state.chats.background()).await.unwrap_or_default()
}

#[tauri::command]
async fn chat_stop_task(app: AppHandle, chat_id: String, task_id: String) -> CmdResult<()> {
    off_main(app, move |_, state| state.chats.stop_task(&chat_id, &task_id)).await?
}

#[tauri::command]
async fn chat_subagent_snapshot(app: AppHandle, chat_id: String, task_id: String) -> chats::SubagentSnapshot {
    off_main(app, move |_, state| state.chats.subagent_snapshot(&chat_id, &task_id)).await.unwrap_or(chats::SubagentSnapshot {
        events: Vec::new(),
        seq: 0,
        busy: false,
    })
}

#[tauri::command]
async fn chat_subagent_send(app: AppHandle, chat_id: String, task_id: String, prompt: Prompt) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.chats.send_subagent(&chat_id, &task_id, prompt)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
async fn chat_subagent_interrupt(app: AppHandle, chat_id: String, task_id: String) -> CmdResult<()> {
    off_main(app, move |_, state| state.chats.interrupt_subagent(&chat_id, &task_id)).await?
}

/// Chats whose text has every word of `query`, newest first.
#[tauri::command]
async fn chat_search(app: AppHandle, query: String) -> CmdResult<Vec<moonfish_agents::search::Hit>> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.chats.index.search(&state.chats.store, &query, 50)
    })
    .await
    .map_err(err)
}

/// The chat as Markdown for people (an issue, a PR description): its title, what was asked and
/// answered, one line per tool, failures' output.
#[tauri::command]
async fn chat_markdown(app: AppHandle, chat_id: String) -> CmdResult<String> {
    off_main(app, move |_, state| {
        let meta = state.chats.store.get(&chat_id).map_err(err)?;
        let snapshot = state.chats.snapshot(&chat_id)?;
        let body = moonfish_agents::transcript::render(&snapshot.events);
        let title = if meta.title.trim().is_empty() { "Chat" } else { meta.title.trim() };
        Ok(format!("# {title}\n\n{body}\n"))
    })
    .await?
}

/// A new, empty chat in `project_id` (its agent starts with the first message).
#[tauri::command]
async fn chat_create(
    app: AppHandle,
    project_id: String,
    agent: Option<AgentKind>,
    model: Option<String>,
    effort: Option<String>,
    fast_mode: Option<bool>,
    cwd: Option<String>,
) -> CmdResult<ChatMeta> {
    off_main(app, move |app, state| create_chat(app, state, NewChat { project_id, agent, model, effort, fast_mode, cwd })).await?
}

/// What `chat_create` (and a phone's `chat.create`) takes.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NewChat {
    project_id: String,
    agent: Option<AgentKind>,
    model: Option<String>,
    effort: Option<String>,
    fast_mode: Option<bool>,
    cwd: Option<String>,
}

pub(crate) fn create_chat(app: &AppHandle, state: &AppState, c: NewChat) -> CmdResult<ChatMeta> {
    let NewChat { project_id, agent, model, effort, fast_mode, cwd } = c;
    let s = state.settings.lock().unwrap().clone();
    let project = s.projects.iter().find(|p| p.id == project_id).ok_or("no such project")?;
    let agent = agent.or(project.default_agent).unwrap_or(s.providers.default_agent);
    let cwd = cwd.as_deref().unwrap_or(&project.path);
    let meta = state.chats.create(&project_id, agent, model.as_deref().unwrap_or(""), effort.as_deref().unwrap_or(""), fast_mode, cwd)?;
    state.chats.changed(app);
    Ok(meta)
}

/// The chat as it is now: its log, the text still streaming, and whether a turn is running.
#[tauri::command]
async fn chat_snapshot(app: AppHandle, chat_id: String) -> CmdResult<chats::Snapshot> {
    off_main(app, move |_, state| state.chats.snapshot(&chat_id)).await?
}

#[tauri::command]
async fn chat_send(
    app: AppHandle,
    chat_id: String,
    prompt: Prompt,
    permission: Option<PermissionMode>,
    mode: Option<chats::SendMode>,
) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap().clone();
        state.chats.send(&app, &settings, &chat_id, prompt, permission, mode.unwrap_or_default())
    })
    .await
    .map_err(err)?
}

#[tauri::command]
async fn chat_queue(app: AppHandle, chat_id: String, command: chats::QueueAction) -> CmdResult<bool> {
    off_main(app, move |app, state| {
        let settings = state.settings.lock().unwrap().clone();
        state.chats.queue_action(app, &settings, &chat_id, command)
    })
    .await?
}

/// `/compact [instructions]`.
#[tauri::command]
async fn chat_compact(app: AppHandle, chat_id: String, instructions: String, permission: Option<PermissionMode>) -> CmdResult<()> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap().clone();
        state.chats.compact(&app, &settings, &chat_id, &instructions, permission)
    })
    .await
    .map_err(err)?
}

/// `/clear`: a fresh agent session from the next message.
#[tauri::command]
async fn chat_clear(app: AppHandle, chat_id: String) -> CmdResult<()> {
    off_main(app, move |app, state| state.chats.clear(app, &chat_id)).await?
}

#[tauri::command]
async fn chat_interrupt(app: AppHandle, chat_id: String) -> CmdResult<()> {
    off_main(app, move |app, state| {
        let settings = state.settings.lock().unwrap().clone();
        state.chats.stop_goal_or_turn(app, &settings, &chat_id)
    })
    .await?
}

#[tauri::command]
async fn chat_goal(app: AppHandle, chat_id: String, command: chats::GoalAction, permission: Option<PermissionMode>) -> CmdResult<()> {
    off_main(app, move |app, state| {
        let settings = state.settings.lock().unwrap().clone();
        state.chats.goal(app, &settings, &chat_id, command, permission)
    })
    .await?
}

#[tauri::command]
async fn chat_answer(app: AppHandle, chat_id: String, request_id: String, decision: Decision) -> CmdResult<()> {
    off_main(app, move |_, state| state.chats.answer(&chat_id, &request_id, decision)).await?
}

#[tauri::command]
async fn chat_answer_question(
    app: AppHandle,
    chat_id: String,
    request_id: String,
    answers: std::collections::BTreeMap<String, Vec<String>>,
) -> CmdResult<()> {
    off_main(app, move |_, state| state.chats.answer_question(&chat_id, &request_id, answers)).await?
}

#[tauri::command]
async fn chat_set_model(
    app: AppHandle,
    chat_id: String,
    model: String,
    effort: Option<String>,
    fast_mode: Option<bool>,
) -> CmdResult<ChatMeta> {
    off_main(app, move |app, state| {
        let meta = state.chats.set_model(&chat_id, Some(&model), effort.as_deref(), fast_mode)?;
        state.chats.changed(app);
        Ok(meta)
    })
    .await?
}

/// An image saved with a chat, as a `data:` URL.
#[tauri::command]
async fn chat_image(app: AppHandle, chat_id: String, file: String) -> CmdResult<String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<AppState>().chats.image(&chat_id, &file)).await.map_err(err)?
}

#[tauri::command]
async fn chat_set_effort(app: AppHandle, chat_id: String, effort: String) -> CmdResult<ChatMeta> {
    off_main(app, move |app, state| {
        let meta = state.chats.set_effort(&chat_id, &effort)?;
        state.chats.changed(app);
        Ok(meta)
    })
    .await?
}

#[tauri::command]
async fn chat_set_permission(app: AppHandle, chat_id: String, mode: PermissionMode) -> CmdResult<()> {
    off_main(app, move |app, state| state.chats.set_permission(app, &chat_id, mode)).await?
}

#[tauri::command]
async fn chat_sleep(app: AppHandle, chat_id: String) -> CmdResult<()> {
    off_main(app, move |app, state| state.chats.sleep(app, &chat_id, std::time::Duration::ZERO)).await?
}

#[tauri::command]
async fn chat_delete(app: AppHandle, chat_id: String) -> CmdResult<()> {
    off_main(app, move |app, state| {
        state.chats.delete(&chat_id)?;
        state.chats.changed(app);
        Ok(())
    })
    .await?
}

#[tauri::command]
async fn chat_rename(app: AppHandle, chat_id: String, title: String) -> CmdResult<ChatMeta> {
    off_main(app, move |app, state| {
        let m = state.chats.edit(&chat_id, |m| m.title = title.trim().to_string())?;
        state.chats.changed(app);
        Ok(m)
    })
    .await?
}

#[tauri::command]
async fn chat_pin(app: AppHandle, chat_id: String, pinned: bool) -> CmdResult<ChatMeta> {
    off_main(app, move |app, state| {
        let m = state.chats.edit(&chat_id, |m| m.pinned = pinned)?;
        // Phones list pinned chats first too.
        state.chats.changed(app);
        Ok(m)
    })
    .await?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // A file that can't be read is moved aside and reported, never saved over.
    let (mut settings, problem) = Settings::load_or_recover(&paths::settings_file());
    if let Some(p) = problem {
        eprintln!("moonfish: {p}");
        let _ = SETTINGS_PROBLEM.set(p);
    }
    // Values typed before the keychain existed move there now.
    let before = settings.clone();
    if secrets::stash(&mut settings, &before).is_ok() && settings != before {
        let _ = settings.save(&paths::settings_file());
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Window size, position and maximized state, remembered between runs.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(AppState {
            settings: Mutex::new(settings),
            chats: chats::Chats::new({
                let store = ChatStore::new(paths::chats_dir());
                store.prune_orphans();
                store
            }),
            remote: remote::Remote::default(),
            logins: LoginManager::default(),
        })
        .setup(|app| {
            if let Ok(dir) = app.path().resource_dir() {
                moonfish_transport::host::set_binary_dir(dir.join("host"));
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(15));
                let state = handle.state::<AppState>();
                let minutes = state.settings.lock().unwrap().agent_sleep_minutes;
                state.chats.sleep_idle(&handle, minutes);
            });
            // A click on a notification opens its chat.
            attention::listen(app.handle());
            // Phones can connect from the start when Settings → Phone has it on.
            remote::start_if_enabled(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            settings_get,
            settings_set,
            project_add,
            project_remove,
            ssh_config_hosts,
            ssh_config_read,
            ssh_config_save,
            location_check,
            location_list_dir,
            wsl_distros,
            save_bytes,
            wsl_start,
            wsl_path,
            provider_probe,
            provider_account,
            provider_limits,
            provider_login_start,
            provider_login_submit,
            provider_login_cancel,
            provider_catalog,
            chat_list,
            chat_waiting,
            chat_background,
            chat_busy,
            chats_stop_all,
            chat_stop_task,
            chat_subagent_snapshot,
            chat_subagent_send,
            chat_subagent_interrupt,
            chat_search,
            chat_markdown,
            attention::app_set_badge,
            chat_create,
            chat_snapshot,
            chat_send,
            chat_queue,
            chat_interrupt,
            chat_goal,
            chat_compact,
            chat_clear,
            chat_answer,
            chat_set_permission,
            chat_set_effort,
            chat_image,
            chat_answer_question,
            chat_set_model,
            chat_sleep,
            chat_delete,
            chat_rename,
            chat_pin,
            remote_firewall::remote_allow_firewall,
            remote::remote_state,
            remote::remote_set_enabled,
            remote::remote_set_port,
            remote::remote_pair_start,
            remote::remote_pair_cancel,
            remote::remote_revoke,
            git::git_status,
            git::git_diff,
            git::git_stage,
            git::git_unstage,
            git::git_discard,
            git::git_commit,
            git::git_branches,
            git::git_switch,
            git::git_sync,
            git::git_log,
            git::git_commit_files,
            git::git_commit_diff,
            git::files_list,
            git::files_grep,
            git::file_read,
            git::file_read_image,
            git::git_suggest_message,
            git::worktree_list,
            git::worktree_create,
            git::worktree_remove,
            git::pr_state,
            git::pr_create,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Moonfish")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<AppState>();
                state.logins.cancel_all();
                state.chats.stop_all();
                // What's still running on SSH hosts ends there too, as it would with plain ssh.
                moonfish_transport::host::shutdown();
            }
        });
}
