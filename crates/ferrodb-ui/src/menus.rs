//! Application menus: native menu bar on macOS, in-window menu bar (rendered
//! in the custom title bar) on Windows and Linux. Rebuilt when the language or
//! developer mode changes.

use ferrodb_ui_kit::{
    gpui::{App, Menu, MenuItem, SystemMenuType},
    kit::GlobalState,
};

use crate::{actions::*, i18n::t, settings::SettingsStore};

fn item(label_id: &str, action: impl ferrodb_ui_kit::gpui::Action) -> MenuItem {
    MenuItem::action(t(label_id), action)
}

/// Menu structure (shared by both presentations).
pub fn build(cx: &App) -> Vec<Menu> {
    let mac = cfg!(target_os = "macos");
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu::new(t("menu-app")).items([
            item("action-about", About),
            MenuItem::separator(),
            item("action-open-settings", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu(t("menu-services"), SystemMenuType::Services),
            MenuItem::separator(),
            item("action-quit", Quit),
        ]));
    }
    let mut file = vec![
        item("action-new-console", NewConsole),
        item("action-new-data-source", NewDataSource),
        MenuItem::separator(),
        item("action-open-project", OpenProject),
        item("action-recent-projects", RecentProjects),
    ];
    if !mac {
        file.extend([
            MenuItem::separator(),
            item("action-open-settings", OpenSettings),
            MenuItem::separator(),
            item("action-quit", Quit),
        ]);
    }
    menus.push(Menu::new(t("menu-file")).items(file));
    menus.push(Menu::new(t("menu-view")).items([
        item("action-find-action", FindAction),
        item("action-search-everywhere", SearchEverywhere),
        MenuItem::separator(),
        item("action-toggle-explorer", ToggleExplorer),
        item("action-toggle-bottom-panel", ToggleBottomPanel),
        item("action-toggle-right-panel", ToggleRightPanel),
        item("action-reset-layout", ResetLayout),
        MenuItem::separator(),
        item("action-toggle-theme", ToggleTheme),
    ]));
    menus.push(Menu::new(t("menu-console")).items([
        item("action-execute", ExecuteStatement),
        item("action-stop", StopQuery),
        MenuItem::separator(),
        item("action-format", FormatCode),
        item("action-rename", Rename),
        item("action-goto-declaration", GoToDeclaration),
        item("action-live-templates", LiveTemplates),
        item("action-quick-doc", QuickDocumentation),
    ]));
    if SettingsStore::get(cx).developer_mode {
        menus.push(Menu::new(t("menu-developer")).items([
            item("action-dev-scenarios", DeveloperScenarios),
            item("action-dev-notification", ShowNotificationDemo),
        ]));
    }
    let mut help = vec![item("action-find-action", FindAction)];
    if !mac {
        help.push(MenuItem::separator());
        help.push(item("action-about", About));
    }
    menus.push(Menu::new(t("menu-help")).items(help));
    menus
}

/// Installs menus for the current platform and language.
pub fn install(cx: &mut App) {
    let menus = build(cx);
    if cfg!(target_os = "macos") {
        cx.set_menus(menus);
    } else {
        GlobalState::global_mut(cx).set_app_menus(menus.into_iter().map(Menu::owned).collect());
    }
}
