//! The action system. Every user-invocable command is an action registered in
//! [`ACTIONS`]: it gets a translated label, a category, an optional default key
//! binding (plan, Appendix B) and is automatically listed in "Find Action".

use ferrodb_ui_kit::{actions, gpui::Action};

actions!(
    ferrodb,
    [
        // application
        Quit,
        About,
        OpenSettings,
        ToggleTheme,
        OpenProject,
        RecentProjects,
        // navigation
        FindAction,
        SearchEverywhere,
        // layout
        ToggleExplorer,
        ToggleBottomPanel,
        ToggleRightPanel,
        ResetLayout,
        // data sources
        NewDataSource,
        // consoles
        NewConsole,
        ExecuteStatement,
        StopQuery,
        FormatCode,
        Rename,
        GoToDeclaration,
        LiveTemplates,
        QuickDocumentation,
        // developer
        DeveloperScenarios,
        ShowNotificationDemo,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Application,
    Navigation,
    Layout,
    DataSources,
    Console,
    Developer,
}

impl Category {
    pub fn label_id(self) -> &'static str {
        match self {
            Self::Application => "category-application",
            Self::Navigation => "category-navigation",
            Self::Layout => "category-layout",
            Self::DataSources => "category-data-sources",
            Self::Console => "category-console",
            Self::Developer => "category-developer",
        }
    }
}

/// Key context of the console editor; console actions are bound there.
pub const CONSOLE_CONTEXT: &str = "Console";
/// Key context of the main window.
pub const WORKSPACE_CONTEXT: &str = "Workspace";

/// Static description of an action.
pub struct ActionSpec {
    /// Fluent message id of the label (`action-…`).
    pub label_id: &'static str,
    pub category: Category,
    pub build: fn() -> Box<dyn Action>,
    /// Default binding on Windows / Linux (`secondary-` = Ctrl/Cmd).
    pub keys: Option<&'static str>,
    /// Default binding on macOS when different from `keys`.
    pub mac_keys: Option<&'static str>,
    pub context: Option<&'static str>,
    /// Extra search terms (not translated: technical names).
    pub keywords: &'static [&'static str],
}

impl ActionSpec {
    pub fn default_keys(&self) -> Option<&'static str> {
        if cfg!(target_os = "macos") {
            self.mac_keys.or(self.keys)
        } else {
            self.keys
        }
    }

    pub fn action_name(&self) -> &'static str {
        (self.build)().name()
    }
}

macro_rules! spec {
    ($ty:ident, $label:literal, $cat:ident, $keys:expr, $mac:expr, $ctx:expr, [$($kw:literal),*]) => {
        ActionSpec {
            label_id: $label,
            category: Category::$cat,
            build: || Box::new($ty),
            keys: $keys,
            mac_keys: $mac,
            context: $ctx,
            keywords: &[$($kw),*],
        }
    };
}

const W: Option<&str> = Some(WORKSPACE_CONTEXT);
const C: Option<&str> = Some(CONSOLE_CONTEXT);

/// All actions of the application, in palette order.
pub static ACTIONS: &[ActionSpec] = &[
    spec!(
        FindAction,
        "action-find-action",
        Navigation,
        Some("secondary-shift-a"),
        None,
        None,
        ["command", "palette"]
    ),
    spec!(
        SearchEverywhere,
        "action-search-everywhere",
        Navigation,
        None,
        None,
        None,
        ["goto", "table", "collection", "key"]
    ),
    spec!(
        OpenSettings,
        "action-open-settings",
        Application,
        Some("ctrl-alt-s"),
        Some("cmd-,"),
        None,
        ["preferences", "options"]
    ),
    spec!(
        ToggleTheme,
        "action-toggle-theme",
        Application,
        None,
        None,
        None,
        ["dark", "light", "appearance"]
    ),
    spec!(
        OpenProject,
        "action-open-project",
        Application,
        None,
        None,
        None,
        ["folder"]
    ),
    spec!(
        RecentProjects,
        "action-recent-projects",
        Application,
        None,
        None,
        None,
        ["folder"]
    ),
    spec!(
        About,
        "action-about",
        Application,
        None,
        None,
        None,
        ["version"]
    ),
    spec!(
        Quit,
        "action-quit",
        Application,
        None,
        Some("cmd-q"),
        None,
        ["exit"]
    ),
    spec!(
        ToggleExplorer,
        "action-toggle-explorer",
        Layout,
        Some("alt-1"),
        Some("cmd-1"),
        None,
        ["database explorer", "tree"]
    ),
    spec!(
        ToggleBottomPanel,
        "action-toggle-bottom-panel",
        Layout,
        Some("alt-4"),
        Some("cmd-4"),
        None,
        ["services", "output"]
    ),
    spec!(
        ToggleRightPanel,
        "action-toggle-right-panel",
        Layout,
        None,
        None,
        None,
        []
    ),
    spec!(
        ResetLayout,
        "action-reset-layout",
        Layout,
        None,
        None,
        None,
        ["dock", "panels"]
    ),
    spec!(
        NewDataSource,
        "action-new-data-source",
        DataSources,
        None,
        None,
        None,
        ["connection", "database"]
    ),
    spec!(
        NewConsole,
        "action-new-console",
        Console,
        Some("secondary-shift-l"),
        None,
        W,
        ["query", "sql"]
    ),
    spec!(
        ExecuteStatement,
        "action-execute",
        Console,
        Some("secondary-enter"),
        None,
        C,
        ["run", "query"]
    ),
    spec!(
        StopQuery,
        "action-stop",
        Console,
        Some("secondary-f2"),
        None,
        C,
        ["cancel"]
    ),
    spec!(
        FormatCode,
        "action-format",
        Console,
        Some("ctrl-alt-l"),
        Some("cmd-alt-l"),
        C,
        ["reformat"]
    ),
    spec!(
        Rename,
        "action-rename",
        Console,
        Some("shift-f6"),
        None,
        C,
        ["refactor"]
    ),
    spec!(
        GoToDeclaration,
        "action-goto-declaration",
        Console,
        Some("secondary-b"),
        None,
        C,
        ["navigate"]
    ),
    spec!(
        LiveTemplates,
        "action-live-templates",
        Console,
        Some("secondary-j"),
        None,
        C,
        ["snippet"]
    ),
    spec!(
        QuickDocumentation,
        "action-quick-doc",
        Console,
        Some("ctrl-q"),
        Some("f1"),
        C,
        ["help"]
    ),
    spec!(
        DeveloperScenarios,
        "action-dev-scenarios",
        Developer,
        None,
        None,
        None,
        ["mock", "debug"]
    ),
    spec!(
        ShowNotificationDemo,
        "action-dev-notification",
        Developer,
        None,
        None,
        None,
        ["toast"]
    ),
];

/// Finds the spec of an action by its qualified name (`ferrodb::FindAction`).
pub fn spec_by_name(name: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|s| s.action_name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_are_unique_and_namespaced() {
        let mut names: Vec<_> = ACTIONS.iter().map(|a| a.action_name()).collect();
        assert!(names.iter().all(|n| n.starts_with("ferrodb::")));
        names.sort();
        let len = names.len();
        names.dedup();
        assert_eq!(len, names.len());
    }

    #[test]
    fn every_action_has_a_translated_label() {
        let en = crate::i18n::message_ids(crate::i18n::Locale::En);
        for a in ACTIONS {
            assert!(
                en.iter().any(|m| m == a.label_id),
                "missing label {}",
                a.label_id
            );
            assert!(en.iter().any(|m| m == a.category.label_id()));
        }
    }
}
