//! Project files panel (plan, phase 7): the files of the project directory.
//! Scripts (`.sql`, `.js`…) open in a console bound to the file (Ctrl+S
//! writes it back, Local History keeps its revisions); data files (CSV,
//! TSV, Parquet, JSON) and DuckDB / SQLite database files open as a data
//! source — data files through DuckDB, each file a view.

use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
};

use ferrodb_api::{DataSourceConfig, DataSourceId, Environment, SecretValues, TxMode};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex,
        list::ListItem,
        notification::Notification,
        tree::{Tree, TreeItem, TreeState},
        v_flex,
    },
};

use super::panel_boilerplate;
use crate::{
    AppState,
    i18n::{t, t_args},
    workspace,
};

pub const NAME: &str = "ProjectFiles";

/// Entries listed at most (large directories are cut).
const MAX_ENTRIES: usize = 5000;
const MAX_DEPTH: usize = 8;
/// Directories never listed.
const SKIPPED: &[&str] = &["target", "node_modules", "__pycache__"];

/// What opening a file does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// Opens in a console bound to the file.
    Script,
    /// CSV / TSV / Parquet / JSON: a DuckDB data source with the file as a view.
    Data,
    /// A DuckDB database file.
    DuckDb,
    /// A SQLite database file.
    Sqlite,
    Other,
}

pub fn file_kind(path: &Path) -> FileKind {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let ext = name.rsplit('.').next().unwrap_or_default();
    let inner = name
        .trim_end_matches(".gz")
        .trim_end_matches(".zst")
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_owned();
    match ext {
        "sql" | "ddl" | "js" | "cql" | "sqlpp" | "partiql" | "redis" | "txt" => FileKind::Script,
        "csv" | "tsv" | "tab" | "parquet" | "json" | "jsonl" | "ndjson" => FileKind::Data,
        "gz" | "zst" if matches!(inner.as_str(), "csv" | "tsv" | "json" | "jsonl") => {
            FileKind::Data
        }
        "duckdb" | "ddb" => FileKind::DuckDb,
        "sqlite" | "sqlite3" | "db" => FileKind::Sqlite,
        _ => FileKind::Other,
    }
}

/// The view DuckDB creates for a data file (same rule as the driver).
pub fn view_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("data")
        .trim_end_matches(".csv")
        .trim_end_matches(".json");
    let mut out: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert_str(0, "t_");
    }
    out
}

/// (driver, properties, query to start with).
pub type FileSourceSpec = (&'static str, Vec<(String, String)>, String);

/// A data source for a file: (driver, properties, query to start with).
pub fn source_for(path: &Path) -> Option<FileSourceSpec> {
    let p = path.to_string_lossy().into_owned();
    match file_kind(path) {
        FileKind::Data => Some((
            "duckdb",
            vec![("files".into(), p)],
            format!("select * from {} limit 100", view_name(path)),
        )),
        FileKind::DuckDb => Some(("duckdb", vec![("file".into(), p)], String::new())),
        FileKind::Sqlite => Some(("sqlite", vec![("file".into(), p)], String::new())),
        _ => None,
    }
}

/// Files and directories under `root`, directories first, both by name.
pub fn scan(root: &Path) -> (Vec<TreeItem>, HashMap<SharedString, PathBuf>) {
    fn walk(
        dir: &Path,
        depth: usize,
        count: &mut usize,
        paths: &mut HashMap<SharedString, PathBuf>,
    ) -> Vec<TreeItem> {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut entries: Vec<(bool, String, PathBuf)> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || SKIPPED.contains(&name.as_str()) {
                    return None;
                }
                let is_dir = e.file_type().ok()?.is_dir();
                Some((is_dir, name, e.path()))
            })
            .collect();
        entries.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        let mut out = Vec::new();
        for (is_dir, name, path) in entries {
            if *count >= MAX_ENTRIES {
                break;
            }
            *count += 1;
            let id: SharedString = path.to_string_lossy().into_owned().into();
            paths.insert(id.clone(), path.clone());
            let mut item = TreeItem::new(id, name);
            if is_dir && depth < MAX_DEPTH {
                item = item.children(walk(&path, depth + 1, count, paths));
            }
            out.push(item);
        }
        out
    }
    let mut paths = HashMap::new();
    let mut count = 0;
    let items = walk(root, 0, &mut count, &mut paths);
    (items, paths)
}

pub struct FilesPanel {
    focus: FocusHandle,
    pub root: Option<PathBuf>,
    tree: Entity<TreeState>,
    paths: Rc<RefCell<HashMap<SharedString, PathBuf>>>,
}

impl FilesPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let mut this = Self {
            focus: cx.focus_handle(),
            root: None,
            tree,
            paths: Rc::default(),
        };
        this.reload(cx);
        this
    }

    /// Reads the project directory again.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.root = workspace::project_root(cx);
        let (items, paths) = self.root.as_deref().map(scan).unwrap_or_default();
        *self.paths.borrow_mut() = paths;
        self.tree.update(cx, |t, cx| t.set_items(items, cx));
        cx.notify();
    }

    /// Paths of the listed files (UI tests).
    pub fn listed(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.paths.borrow().values().cloned().collect();
        v.sort();
        v
    }

    fn selected(&self, cx: &App) -> Option<PathBuf> {
        let id = self.tree.read(cx).selected_item()?.id.clone();
        self.paths.borrow().get(&id).cloned()
    }

    /// Opens a file by its kind.
    pub fn open(path: PathBuf, window: &mut Window, cx: &mut App) {
        match file_kind(&path) {
            FileKind::Script | FileKind::Other => {
                let ds = workspace::active_console_source(cx)
                    .or_else(|| workspace::explorer_selection(cx).map(|n| n.data_source));
                if let Err(e) = workspace::open_file_console(ds, path, window, cx) {
                    window.push_notification(Notification::error(e.to_string()), cx);
                }
            }
            _ => Self::open_as_source(path, window, cx),
        }
    }

    /// A data source for a data or database file (reused when one exists),
    /// then a console on it.
    pub fn open_as_source(path: PathBuf, window: &mut Window, cx: &mut App) {
        let Some((driver, props, query)) = source_for(&path) else {
            return;
        };
        let backend = AppState::backend(cx);
        let existing = backend
            .data_sources()
            .list()
            .into_iter()
            .find(|s| s.config.driver.as_str() == driver && s.config.properties == props)
            .map(|s| s.config.id);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let id = existing.clone().unwrap_or_else(|| {
            let base: String = name
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect();
            let ids: Vec<DataSourceId> = backend
                .data_sources()
                .list()
                .into_iter()
                .map(|s| s.config.id)
                .collect();
            let mut id = DataSourceId::new(format!("file-{base}"));
            let mut n = 2;
            while ids.contains(&id) {
                id = DataSourceId::new(format!("file-{base}-{n}"));
                n += 1;
            }
            id
        });
        let cfg = DataSourceConfig {
            id: id.clone(),
            name,
            driver: driver.into(),
            folder: vec![t("files-folder")],
            environment: Environment::Development,
            read_only: false,
            tx_mode: TxMode::Auto,
            properties: props,
            secret_ref: None,
        };
        let task = cx.background_executor().spawn(async move {
            if existing.is_none() {
                backend
                    .data_sources()
                    .save(cfg, SecretValues::new())
                    .await?;
            }
            Ok::<_, ferrodb_api::Error>(())
        });
        window
            .spawn(cx, async move |cx| {
                let r = task.await;
                let _ = cx.update(|window, cx| match r {
                    Ok(()) => {
                        window.push_notification(
                            Notification::success(t_args(
                                "files-opened-as-source",
                                &[("name", id.to_string().into())],
                            )),
                            cx,
                        );
                        if query.is_empty() {
                            workspace::open_console(Some(id), String::new(), window, cx);
                        } else {
                            workspace::open_data_query(id, query, window, cx);
                        }
                    }
                    Err(e) => window.push_notification(Notification::error(e.to_string()), cx),
                });
            })
            .detach();
    }
}

panel_boilerplate!(FilesPanel);

impl BasePanel for FilesPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for FilesPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-files")
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let selected = self.selected(cx);
        let kind = selected.as_deref().map(file_kind);
        let paths = self.paths.clone();
        let tree = Tree::new(&self.tree, move |ix, entry, selected, _, cx| {
            let id = entry.item().id.clone();
            let path = paths.borrow().get(&id).cloned();
            let muted = cx.theme().muted_foreground;
            let is_dir = entry.is_folder();
            let icon = match path.as_deref().map(file_kind) {
                _ if is_dir => icons::Lucide::Folder,
                Some(FileKind::Script) => icons::Lucide::FileCode,
                Some(FileKind::Data) => icons::Lucide::Sheet,
                Some(FileKind::DuckDb | FileKind::Sqlite) => icons::Lucide::Database,
                _ => icons::Lucide::File,
            };
            ListItem::new(ix)
                .selected(selected)
                .pl(px(6. + 14. * entry.depth() as f32))
                .when_some(path.filter(|_| !is_dir), |item, p| {
                    item.on_click(move |ev, window, cx| {
                        if ev.click_count() == 2 {
                            FilesPanel::open(p.clone(), window, cx);
                        }
                    })
                })
                .child(
                    h_flex()
                        .gap_1()
                        .child(icons::Icon::from(icon).xsmall().text_color(muted))
                        .child(entry.item().label.clone()),
                )
        })
        .size_full();
        let open_target = selected.clone().filter(|p| p.is_file());
        let source_target = open_target.clone().filter(|_| {
            matches!(
                kind,
                Some(FileKind::Data | FileKind::DuckDb | FileKind::Sqlite)
            )
        });
        v_flex()
            .id("project-files")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .text_sm()
            .child(
                h_flex()
                    .px_1()
                    .py_0p5()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("files-refresh")
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::RefreshCw)
                            .tooltip(t("files-refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(
                        Button::new("files-open")
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::SquareTerminal)
                            .tooltip(t("files-open"))
                            .disabled(open_target.is_none())
                            .on_click(move |_, w, cx| {
                                if let Some(p) = open_target.clone() {
                                    FilesPanel::open(p, w, cx)
                                }
                            }),
                    )
                    .child(
                        Button::new("files-source")
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::Database)
                            .tooltip(t("files-open-as-source"))
                            .disabled(source_target.is_none())
                            .on_click(move |_, w, cx| {
                                if let Some(p) = source_target.clone() {
                                    FilesPanel::open_as_source(p, w, cx)
                                }
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(
                                self.root
                                    .as_ref()
                                    .map(|r| r.display().to_string())
                                    .unwrap_or_default(),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(tree))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn kinds_views_and_sources() {
        assert_eq!(file_kind(Path::new("a/q.sql")), FileKind::Script);
        assert_eq!(file_kind(Path::new("Sales.CSV")), FileKind::Data);
        assert_eq!(file_kind(Path::new("x.csv.gz")), FileKind::Data);
        assert_eq!(file_kind(Path::new("x.tar.gz")), FileKind::Other);
        assert_eq!(file_kind(Path::new("w.duckdb")), FileKind::DuckDb);
        assert_eq!(file_kind(Path::new("app.db")), FileKind::Sqlite);
        assert_eq!(view_name(Path::new("Sales 2025.csv")), "sales_2025");
        let (driver, props, q) = source_for(Path::new("/d/orders.parquet")).unwrap();
        assert_eq!(driver, "duckdb");
        assert_eq!(props[0].0, "files");
        assert_eq!(q, "select * from orders limit 100");
        assert!(source_for(Path::new("q.sql")).is_none());
    }

    #[::core::prelude::v1::test]
    fn scan_lists_dirs_first_and_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sql/reports")).unwrap();
        std::fs::create_dir_all(dir.path().join(".ferrodb")).unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("a.csv"), "x\n1\n").unwrap();
        std::fs::write(dir.path().join("sql/reports/r.sql"), "select 1").unwrap();
        let (items, paths) = scan(dir.path());
        let names: Vec<String> = items.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(names, vec!["sql", "a.csv"]);
        assert_eq!(paths.len(), 4);
    }
}
