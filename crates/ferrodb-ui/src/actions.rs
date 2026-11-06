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
        EditDataSource,
        OpenDriverManager,
        RefreshExplorer,
        // consoles
        NewConsole,
        ExecuteStatement,
        ExecuteScript,
        ExplainPlan,
        StopQuery,
        FormatCode,
        Rename,
        GoToDeclaration,
        LiveTemplates,
        QuickDocumentation,
        // results
        CopySelection,
        FindInResults,
        SubmitChanges,
        RevertChanges,
        LoadMore,
        ExportData,
        ImportData,
        // tools
        ShowQueryHistory,
        ShowLocalHistory,
        NewTable,
        ShowErDiagram,
        CompareSchemas,
        // developer
        DeveloperScenarios,
        OpenGallery,
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
    Results,
    Tools,
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
            Self::Results => "category-results",
            Self::Tools => "category-tools",
            Self::Developer => "category-developer",
        }
    }
}

/// Key context of the console editor; console actions are bound there.
pub const CONSOLE_CONTEXT: &str = "Console";
/// Key context of the result grid; grid actions are bound there.
pub const GRID_CONTEXT: &str = "ResultGrid";
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
const G: Option<&str> = Some(GRID_CONTEXT);

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
        EditDataSource,
        "action-edit-data-source",
        DataSources,
        None,
        None,
        None,
        ["properties", "connection"]
    ),
    spec!(
        OpenDriverManager,
        "action-driver-manager",
        DataSources,
        None,
        None,
        None,
        ["jdbc", "adbc", "download", "jar"]
    ),
    spec!(
        RefreshExplorer,
        "action-refresh",
        DataSources,
        Some("secondary-f5"),
        None,
        None,
        ["reload", "synchronize"]
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
        ExecuteScript,
        "action-execute-script",
        Console,
        Some("secondary-shift-enter"),
        None,
        C,
        ["run all", "script"]
    ),
    spec!(
        ExplainPlan,
        "action-explain",
        Console,
        Some("secondary-shift-e"),
        None,
        C,
        ["plan", "explain"]
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
        CopySelection,
        "action-copy-selection",
        Results,
        Some("secondary-c"),
        None,
        G,
        ["clipboard"]
    ),
    spec!(
        FindInResults,
        "action-find-in-results",
        Results,
        Some("secondary-f"),
        None,
        G,
        ["search", "filter"]
    ),
    spec!(
        SubmitChanges,
        "action-submit-changes",
        Results,
        Some("secondary-enter"),
        None,
        G,
        ["commit", "save", "apply"]
    ),
    spec!(
        RevertChanges,
        "action-revert-changes",
        Results,
        Some("secondary-alt-z"),
        None,
        G,
        ["undo", "discard"]
    ),
    spec!(
        LoadMore,
        "action-load-more",
        Results,
        None,
        None,
        G,
        ["page", "fetch"]
    ),
    spec!(
        ExportData,
        "action-export-data",
        Results,
        None,
        None,
        None,
        ["csv", "json", "excel", "dump"]
    ),
    spec!(
        ImportData,
        "action-import-data",
        Results,
        None,
        None,
        None,
        ["csv", "load"]
    ),
    spec!(
        ShowQueryHistory,
        "action-query-history",
        Tools,
        None,
        None,
        None,
        ["history", "recent queries"]
    ),
    spec!(
        ShowLocalHistory,
        "action-local-history",
        Tools,
        None,
        None,
        None,
        ["revisions", "restore"]
    ),
    spec!(
        NewTable,
        "action-new-table",
        Tools,
        None,
        None,
        None,
        ["ddl", "create table", "modify"]
    ),
    spec!(
        ShowErDiagram,
        "action-er-diagram",
        Tools,
        None,
        None,
        None,
        ["diagram", "relations", "foreign keys"]
    ),
    spec!(
        CompareSchemas,
        "action-compare",
        Tools,
        None,
        None,
        None,
        ["diff", "compare"]
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
        OpenGallery,
        "action-dev-gallery",
        Developer,
        None,
        None,
        None,
        ["components", "gallery"]
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
