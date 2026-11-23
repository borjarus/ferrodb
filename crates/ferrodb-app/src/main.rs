//! FerroDb application entry point: startup, window, menus, keymap.
//!
//! Usage: `ferrodb [--mock] [PROJECT_DIR]`. Without a directory the last
//! project (or the default project in the data directory) is opened.
//!
//! The backend is `ferrodb-core` (real databases). `--mock` or
//! `FERRODB_BACKEND=mock` starts the mock backend with its developer
//! scenarios instead (demos, UI work without databases).
//!
//! `ferrodb mcp` runs no GUI: it is the stdio bridge to the MCP server of a
//! running FerroDb, for agents that launch a command (see [`run_mcp_bridge`]).
//! `ferrodb --version` prints the version and exits.
//!
//! Phase 10: the panic hook (crash reports in `<data>/crashes`) and the log
//! ring buffer are installed before anything else; the backend is wrapped
//! in `ReleaseBackend` (updates, crash reports).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use ferrodb_api::Backend;
use ferrodb_core::{CoreBackend, CoreOptions};
use ferrodb_mock::{MockBackend, Scenario};
use ferrodb_ui::{
    AppPaths, AppState, ScenarioControl, Workspace, actions, i18n::t, keymap, output, project,
    settings::SettingsStore,
};
use ferrodb_ui_kit::{
    gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size},
    kit::TitleBar,
};

/// Exposes mock scenarios to the developer menu.
struct MockScenarios(MockBackend);

impl ScenarioControl for MockScenarios {
    fn scenarios(&self) -> Vec<&'static str> {
        Scenario::ALL.iter().map(|s| s.id()).collect()
    }
    fn current(&self) -> &'static str {
        self.0.scenario().id()
    }
    fn set(&self, id: &str) {
        if let Some(s) = Scenario::from_id(id) {
            self.0.set_scenario(s);
        }
    }
}

/// Logs to stderr, and the last lines (masked) into the ring buffer that
/// crash reports include.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::try_from_env("FERRODB_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .with(ferrodb_release::log_layer())
        .try_init();
}

/// Where the mock keeps crash reports (nothing in the user's data).
fn mock_data_dir() -> PathBuf {
    std::env::temp_dir().join("ferrodb-mock")
}

fn app_paths() -> anyhow::Result<AppPaths> {
    if let Ok(dir) = std::env::var("FERRODB_HOME") {
        let dir = PathBuf::from(dir);
        return Ok(AppPaths {
            config_dir: dir.join("config"),
            data_dir: dir.join("data"),
        });
    }
    let dirs = directories::ProjectDirs::from("dev", "FerroDb", "FerroDb")
        .context("cannot determine the home directory")?;
    Ok(AppPaths {
        config_dir: dirs.config_dir().to_path_buf(),
        data_dir: dirs.data_dir().to_path_buf(),
    })
}

fn install_keymap(paths: &AppPaths, cx: &mut App) {
    let mut user = keymap::KeymapFile::default();
    let mut problems = Vec::new();
    if let Ok(text) = std::fs::read_to_string(paths.keymap_file()) {
        match keymap::parse(&text) {
            Ok(k) => user = k,
            Err(e) => problems.push(format!("keymap.toml: {e}")),
        }
    }
    let bindings = keymap::merge(keymap::default_bindings(), &user);
    problems.extend(keymap::install(&bindings, cx));
    for p in problems {
        output::log(output::Level::Warning, p, cx);
    }
}

fn use_mock() -> bool {
    std::env::args().any(|a| a == "--mock")
        || std::env::var("FERRODB_BACKEND").is_ok_and(|v| v.eq_ignore_ascii_case("mock"))
}

fn project_root(paths: &AppPaths, cx: &App) -> PathBuf {
    if let Some(arg) = std::env::args_os().skip(1).find(|a| a != "--mock") {
        return PathBuf::from(arg);
    }
    SettingsStore::get(cx)
        .last_project
        .clone()
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| project::default_project_root(&paths.data_dir))
}

fn open_main_window(root: PathBuf, cx: &mut App) -> anyhow::Result<()> {
    let project = project::Project::open(&root)
        .with_context(|| format!("open project {}", root.display()))?;
    SettingsStore::update(cx, |s| s.push_recent(&project.root));
    let title = format!("{} — {}", project.name(), t("app-name"));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.), px(820.)),
            cx,
        ))),
        window_min_size: Some(size(px(800.), px(500.))),
        app_id: Some("dev.ferrodb.app".into()),
        ..TitleBar::window_options()
    };
    ferrodb_ui_kit::open_window(options, cx, move |window, cx| {
        window.set_window_title(&title);
        let ws = cx.new(|cx| Workspace::new(project, window, cx));
        let weak = ws.downgrade();
        window.on_window_should_close(cx, move |_, cx| {
            let _ = weak.update(cx, |ws, cx| ws.save_layout_now(cx));
            true
        });
        ws
    })?;
    Ok(())
}

/// `ferrodb mcp`: an MCP server on stdin/stdout for clients that launch a
/// command (Claude Code, Claude Desktop), forwarding to the MCP endpoint of
/// the running IDE. Logs go to stderr: stdout carries the protocol.
///
/// Release builds on Windows use the `windows` subsystem (no console of
/// their own); the pipes a client passes as stdin/stdout are inherited and
/// work all the same.
fn run_mcp_bridge() -> anyhow::Result<()> {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_env("FERRODB_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
    let paths = app_paths()?;
    if let Err(e) = ferrodb_ai::mcp::run_stdio_bridge(&paths.config_dir) {
        eprintln!("ferrodb mcp: {e}");
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let first = std::env::args_os().nth(1);
    if first
        .as_ref()
        .is_some_and(|a| a == "--version" || a == "-V")
    {
        println!(
            "ferrodb {} (API v{}, {} channel)",
            ferrodb_release::VERSION,
            ferrodb_api::API_VERSION,
            ferrodb_release::build_channel()
        );
        return Ok(());
    }
    if first.is_some_and(|a| a == "mcp") {
        return run_mcp_bridge();
    }
    let paths = app_paths()?;
    // Before anything else: every later panic leaves a (masked) report.
    let crash_data = if use_mock() {
        mock_data_dir()
    } else {
        paths.data_dir.clone()
    };
    ferrodb_release::install_panic_hook(&crash_data, ferrodb_release::VERSION);
    init_tracing();
    let (backend, scenarios): (Arc<dyn Backend>, Option<Arc<dyn ScenarioControl>>) = if use_mock() {
        let mock = MockBackend::new();
        let scenarios: Arc<dyn ScenarioControl> = Arc::new(MockScenarios(mock.clone()));
        (Arc::new(mock), Some(scenarios))
    } else {
        let core = CoreBackend::new(CoreOptions::system(
            paths.config_dir.clone(),
            paths.data_dir.clone(),
        ))
        .context("start the core")?;
        (Arc::new(core), None)
    };
    // Phase 9: the assistant and the agents (MCP server) on top of either backend.
    let backend: Arc<dyn Backend> = ferrodb_ai::AiBackend::new(
        backend,
        if use_mock() {
            ferrodb_ai::AiOptions::in_memory()
        } else {
            ferrodb_ai::AiOptions {
                config_dir: Some(paths.config_dir.clone()),
                data_dir: Some(paths.data_dir.clone()),
                secrets: Arc::from(ferrodb_secrets::system_store("FerroDb")),
            }
        },
    );
    // Phase 10: signed updates and crash reports on top of either backend
    // (the mock keeps its settings in memory).
    let backend: Arc<dyn Backend> = ferrodb_release::ReleaseBackend::new(
        backend,
        if use_mock() {
            ferrodb_release::ReleaseOptions::from_build(None, Some(crash_data))
        } else {
            ferrodb_release::ReleaseOptions::from_build(
                Some(paths.config_dir.clone()),
                Some(paths.data_dir.clone()),
            )
        },
    );
    tracing::info!(
        version = ferrodb_release::VERSION,
        backend = backend.name(),
        config = %paths.config_dir.display(),
        data = %paths.data_dir.display(),
        "starting FerroDb"
    );

    ferrodb_ui_kit::application().run(move |cx: &mut App| {
        ferrodb_ui::init(
            AppState {
                backend,
                scenarios: scenarios.clone(),
                paths: Some(paths.clone()),
            },
            cx,
        );
        if let Some(scenarios) = &scenarios {
            let saved_scenario = SettingsStore::get(cx).dev_scenario.clone();
            scenarios.set(&saved_scenario);
        }
        install_keymap(&paths, cx);

        cx.on_action(|_: &actions::Quit, cx: &mut App| {
            if let Some(ws) = ferrodb_ui::workspace::active(cx) {
                ws.update(cx, |ws, cx| ws.save_layout_now(cx));
            }
            cx.quit();
        });
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let root = project_root(&paths, cx);
        if let Err(e) = open_main_window(root, cx) {
            tracing::error!(error = %e, "failed to open the main window");
            cx.quit();
        }
        cx.activate(true);
    });
    Ok(())
}
