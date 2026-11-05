//! FerroDb application entry point: startup, window, menus, keymap.
//!
//! Usage: `ferrodb [PROJECT_DIR]`. Without an argument the last project (or
//! the default project in the data directory) is opened.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{path::PathBuf, sync::Arc};

use anyhow::Context as _;
use ferrodb_api::Backend;
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

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_env("FERRODB_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
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

fn project_root(paths: &AppPaths, cx: &App) -> PathBuf {
    if let Some(arg) = std::env::args_os().nth(1) {
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

fn main() -> anyhow::Result<()> {
    init_tracing();
    let paths = app_paths()?;
    let mock = MockBackend::new();
    let scenarios: Arc<dyn ScenarioControl> = Arc::new(MockScenarios(mock.clone()));
    let backend: Arc<dyn Backend> = Arc::new(mock);
    tracing::info!(config = %paths.config_dir.display(), data = %paths.data_dir.display(), "starting FerroDb");

    ferrodb_ui_kit::application().run(move |cx: &mut App| {
        ferrodb_ui::init(
            AppState {
                backend,
                scenarios: Some(scenarios.clone()),
                paths: Some(paths.clone()),
            },
            cx,
        );
        let saved_scenario = SettingsStore::get(cx).dev_scenario.clone();
        scenarios.set(&saved_scenario);
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
