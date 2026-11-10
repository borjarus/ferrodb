//! Database Explorer (plan 2.1): data sources in folders, and below each
//! source a lazily loaded object tree whose levels come from the source's
//! `HierarchySpec`. Pattern filter, environment colors, type icons, kind
//! groups ("tables", "views"…) and a context menu that depends on the data
//! model and capabilities.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use ferrodb_api::{
    Capabilities, ConnectionState, DataModel, DataSourceId, DataSourceInfo, DbObject, Environment,
    ExplainSupport, NodeRef, ObjectKind, RefreshScope, SchemaKind, ScriptKind,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex,
        input::{Input, InputEvent, InputState},
        list::ListItem,
        menu::PopupMenuItem,
        notification::Notification,
        tree::{Tree, TreeEvent, TreeItem, TreeState},
        v_flex,
    },
    theme::environment_color,
};

use super::panel_boilerplate;
use crate::{
    AppState, actions,
    i18n::{t, t_args},
    inspector::{self, Inspection},
    output, workspace,
};

pub const NAME: &str = "Explorer";

/// Kinds that open data on double-click.
pub fn opens_data(kind: ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Table
            | ObjectKind::View
            | ObjectKind::MaterializedView
            | ObjectKind::Collection
            | ObjectKind::Key
            | ObjectKind::KeyPrefix
    )
}

/// Message id of a kind group ("tables", "views"…).
fn group_label(kind: ObjectKind) -> String {
    t(&format!("group-{}", format!("{kind:?}").to_lowercase()))
}

#[derive(Clone)]
pub enum Row {
    Folder(String),
    Source(DataSourceInfo),
    Group { kind: ObjectKind, count: usize },
    Object { object: DbObject, env: Environment },
    Loading,
    Failed(String),
}

#[derive(Clone)]
enum Children {
    Loading,
    Loaded(Vec<DbObject>),
    Failed(String),
}

pub struct ExplorerPanel {
    focus: FocusHandle,
    tree: Entity<TreeState>,
    filter: Entity<InputState>,
    rows: Rc<RefCell<HashMap<SharedString, Row>>>,
    children: HashMap<NodeRef, Children>,
    expanded: HashSet<SharedString>,
    /// Folders and kind groups start expanded; these were collapsed.
    collapsed: HashSet<SharedString>,
    sources: Vec<DataSourceInfo>,
    /// Node being revealed ("Go to declaration"): parents load first.
    reveal: Option<NodeRef>,
    _subs: Vec<Subscription>,
    _events: Task<()>,
}

fn source_id(ds: &DataSourceId) -> SharedString {
    format!("ds:{ds}").into()
}

fn node_id(node: &NodeRef) -> SharedString {
    let mut s = format!("obj:{}", node.data_source);
    for (k, n) in &node.path {
        s.push_str(&format!("/{k:?}:{n}"));
    }
    s.into()
}

impl ExplorerPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder(t("explorer-filter")));
        let rx = AppState::backend(cx).data_sources().subscribe();
        let events = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        });
        let subs = vec![
            cx.subscribe(&tree, |this, _, ev: &TreeEvent, cx| match ev {
                TreeEvent::Expanded(id) => this.expand(id.clone(), cx),
                TreeEvent::Collapsed(id) => {
                    this.expanded.remove(id);
                    this.collapsed.insert(id.clone());
                }
            }),
            cx.observe(&tree, |this, tree, cx| {
                let id = tree.read(cx).selected_item().map(|i| i.id.clone());
                if let Some(id) = id {
                    this.inspect(&id, cx);
                }
            }),
            cx.subscribe(&filter, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.rebuild(cx);
                }
            }),
        ];
        cx.set_global(workspace::ActiveExplorer(cx.entity().downgrade()));
        let mut this = Self {
            focus: cx.focus_handle(),
            tree,
            filter,
            rows: Rc::default(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            sources: Vec::new(),
            reveal: None,
            _subs: subs,
            _events: events,
        };
        this.reload(cx);
        this
    }

    pub fn tree(&self) -> &Entity<TreeState> {
        &self.tree
    }

    pub fn filter(&self) -> &Entity<InputState> {
        &self.filter
    }

    /// Row behind a tree item id (tests, context menu).
    pub fn row(&self, id: &str) -> Option<Row> {
        self.rows.borrow().get(id).cloned()
    }

    /// Ids of the visible tree entries, in order.
    pub fn visible_ids(&self, cx: &App) -> Vec<SharedString> {
        let tree = self.tree.read(cx);
        (0..)
            .map_while(|i| tree.entry(i).map(|e| e.item().id.clone()))
            .collect()
    }

    /// Reloads data sources (after add / remove / state change).
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.sources = AppState::backend(cx).data_sources().list();
        let ids: HashSet<_> = self.sources.iter().map(|s| s.config.id.clone()).collect();
        self.children.retain(|n, _| ids.contains(&n.data_source));
        self.rebuild(cx);
    }

    /// Expands an item: loads object children on first expansion.
    pub fn expand(&mut self, id: SharedString, cx: &mut Context<Self>) {
        self.expanded.insert(id.clone());
        self.collapsed.remove(&id);
        let node = match self.rows.borrow().get(&id) {
            Some(Row::Source(s)) => Some((NodeRef::root(s.config.id.clone()), Some(s.clone()))),
            Some(Row::Object { object, .. }) => Some((object.node.clone(), None)),
            _ => None,
        };
        let Some((node, source)) = node else {
            self.rebuild(cx);
            return;
        };
        if matches!(
            self.children.get(&node),
            Some(Children::Loaded(_) | Children::Loading)
        ) {
            self.rebuild(cx);
            return;
        }
        self.load(node, source, cx);
    }

    fn load(&mut self, node: NodeRef, source: Option<DataSourceInfo>, cx: &mut Context<Self>) {
        self.children.insert(node.clone(), Children::Loading);
        self.rebuild(cx);
        let backend = AppState::backend(cx);
        // The backend connects when it needs to: children of a cached
        // catalog need no connection (phase 4).
        let _ = source;
        let task = cx.background_executor().spawn({
            let node = node.clone();
            async move { backend.metadata().children(node).await }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let state = match result {
                    Ok(v) => Children::Loaded(v),
                    Err(e) => {
                        output::log(
                            output::Level::Error,
                            format!("{}: {e}", node.data_source),
                            cx,
                        );
                        Children::Failed(e.to_string())
                    }
                };
                this.children.insert(node, state);
                this.rebuild(cx);
                this.advance_reveal(cx);
            });
        })
        .detach();
    }

    /// Expands the parents of `node`, loading them as needed, then selects it.
    pub fn reveal(&mut self, node: NodeRef, cx: &mut Context<Self>) {
        self.reveal = Some(node);
        self.advance_reveal(cx);
    }

    fn advance_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.reveal.clone() else {
            return;
        };
        for depth in 0..target.path.len() {
            let parent = NodeRef {
                data_source: target.data_source.clone(),
                path: target.path[..depth].to_vec(),
            };
            let id = if depth == 0 {
                source_id(&parent.data_source)
            } else {
                node_id(&parent)
            };
            self.expanded.insert(id);
            match self.children.get(&parent) {
                Some(Children::Loaded(_)) => {}
                Some(Children::Loading) => return,
                Some(Children::Failed(_)) => {
                    self.reveal = None;
                    return;
                }
                None => {
                    self.load(parent, None, cx);
                    return;
                }
            }
        }
        self.reveal = None;
        self.rebuild(cx);
        let id = node_id(&target);
        self.tree.update(cx, |tree, cx| {
            if let Some(ix) = tree.index_of(&id) {
                tree.set_selected_index(Some(ix), cx);
                tree.scroll_to_item(ix, ScrollStrategy::Center);
            }
        });
        self.inspect(&id, cx);
    }

    /// Re-reads metadata of a node (or of every loaded source) in the
    /// backend, then reloads the expanded part of the tree.
    pub fn refresh(&mut self, node: Option<NodeRef>, cx: &mut Context<Self>) {
        let targets: Vec<(DataSourceId, RefreshScope)> = match &node {
            Some(n) if n.path.is_empty() => vec![(n.data_source.clone(), RefreshScope::All)],
            Some(n) => vec![(n.data_source.clone(), RefreshScope::Node(n.clone()))],
            None => {
                let mut ids: Vec<DataSourceId> = self
                    .children
                    .keys()
                    .map(|n| n.data_source.clone())
                    .collect();
                ids.sort();
                ids.dedup();
                ids.into_iter().map(|d| (d, RefreshScope::All)).collect()
            }
        };
        let backend = AppState::backend(cx);
        let task = cx.background_executor().spawn(async move {
            let mut errors = Vec::new();
            for (ds, scope) in targets {
                if let Err(e) = backend.metadata().refresh(&ds, scope).await {
                    errors.push(format!("{ds}: {e}"));
                }
            }
            errors
        });
        cx.spawn(async move |this, cx| {
            let errors = task.await;
            let _ = this.update(cx, |this, cx| {
                for e in errors {
                    output::log(output::Level::Error, e, cx);
                }
                this.reload_children(node, cx);
            });
        })
        .detach();
    }

    /// Drops loaded children of a source (or node) and reloads expanded ones.
    fn reload_children(&mut self, node: Option<NodeRef>, cx: &mut Context<Self>) {
        match &node {
            Some(n) => self
                .children
                .retain(|k, _| !(k.data_source == n.data_source && k.path.starts_with(&n.path))),
            None => self.children.clear(),
        }
        let expanded: Vec<SharedString> = self.expanded.iter().cloned().collect();
        self.rebuild(cx);
        for id in expanded {
            self.expand(id, cx);
        }
    }

    /// Sets the filter text (also what typing into the filter field does).
    pub fn set_filter(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.filter
            .update(cx, |i, cx| i.set_value(text, window, cx));
        self.rebuild(cx);
    }

    fn filter_text(&self, cx: &App) -> String {
        self.filter.read(cx).value().trim().to_lowercase()
    }

    /// Rebuilds tree items from sources and loaded children, keeping
    /// expansion; with a filter, only matching branches remain (expanded).
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let filter = self.filter_text(cx);
        let mut rows = HashMap::new();
        let mut folders: Vec<(String, Vec<TreeItem>)> = Vec::new();
        let mut top = Vec::new();
        for s in &self.sources {
            let Some(item) = self.source_item(s, &filter, &mut rows) else {
                continue;
            };
            match s.config.folder.first() {
                Some(f) => match folders.iter_mut().find(|(n, _)| n == f) {
                    Some((_, v)) => v.push(item),
                    None => folders.push((f.clone(), vec![item])),
                },
                None => top.push(item),
            }
        }
        let mut items: Vec<TreeItem> = folders
            .into_iter()
            .map(|(name, children)| {
                let id: SharedString = format!("folder:{name}").into();
                rows.insert(id.clone(), Row::Folder(name.clone()));
                let open = !filter.is_empty() || !self.is_collapsed_folder(&id);
                TreeItem::new(id, name).expanded(open).children(children)
            })
            .collect();
        items.extend(top);
        *self.rows.borrow_mut() = rows;
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        cx.notify();
    }

    fn is_collapsed_folder(&self, id: &SharedString) -> bool {
        self.collapsed.contains(id)
    }

    fn source_item(
        &self,
        s: &DataSourceInfo,
        filter: &str,
        rows: &mut HashMap<SharedString, Row>,
    ) -> Option<TreeItem> {
        let id = source_id(&s.config.id);
        let root = NodeRef::root(s.config.id.clone());
        let name_match = filter.is_empty() || s.config.name.to_lowercase().contains(filter);
        let children = self.object_items(&root, s, if name_match { "" } else { filter }, rows);
        if !name_match && children.as_ref().is_none_or(|c| c.is_empty()) {
            return None;
        }
        rows.insert(id.clone(), Row::Source(s.clone()));
        let expanded = self.expanded.contains(&id) || (!filter.is_empty() && !name_match);
        Some(
            TreeItem::new(id, s.config.name.clone())
                .expanded(expanded)
                .children(children.unwrap_or_else(|| vec![placeholder(&root, rows)])),
        )
    }

    /// Items under `node`; `None` when not loaded yet. With a filter, only
    /// matching objects (and containers with matching loaded descendants).
    fn object_items(
        &self,
        node: &NodeRef,
        s: &DataSourceInfo,
        filter: &str,
        rows: &mut HashMap<SharedString, Row>,
    ) -> Option<Vec<TreeItem>> {
        let objs = match self.children.get(node) {
            None => return None,
            Some(Children::Loading) => {
                let id: SharedString = format!("loading:{}", node_id(node)).into();
                rows.insert(id.clone(), Row::Loading);
                return Some(vec![TreeItem::new(id, t("loading")).disabled(true)]);
            }
            Some(Children::Failed(e)) => {
                let id: SharedString = format!("error:{}", node_id(node)).into();
                rows.insert(id.clone(), Row::Failed(e.clone()));
                return Some(vec![TreeItem::new(id, e.clone()).disabled(true)]);
            }
            Some(Children::Loaded(v)) => v,
        };
        let env = s.config.environment;
        let mut by_kind: Vec<(ObjectKind, Vec<TreeItem>)> = Vec::new();
        for o in objs {
            let id = node_id(&o.node);
            let matches = filter.is_empty() || o.name.to_lowercase().contains(filter);
            let sub = if o.has_children {
                self.object_items(&o.node, s, if matches { "" } else { filter }, rows)
            } else {
                Some(Vec::new())
            };
            if !matches && sub.as_ref().is_none_or(|c| c.is_empty()) {
                continue;
            }
            rows.insert(
                id.clone(),
                Row::Object {
                    object: o.clone(),
                    env,
                },
            );
            let mut item = TreeItem::new(id.clone(), o.name.clone());
            if o.has_children {
                let open = self.expanded.contains(&id) || (!filter.is_empty() && !matches);
                item = item
                    .expanded(open)
                    .children(sub.unwrap_or_else(|| vec![placeholder(&o.node, rows)]));
            }
            match by_kind.iter_mut().find(|(k, _)| *k == o.kind) {
                Some((_, v)) => v.push(item),
                None => by_kind.push((o.kind, vec![item])),
            }
        }
        // Several kinds under one node: group them ("tables (24)", "views (1)").
        if by_kind.len() > 1 && by_kind.iter().any(|(_, v)| v.len() > 1) {
            Some(
                by_kind
                    .into_iter()
                    .map(|(kind, items)| {
                        let id: SharedString = format!("grp:{}:{kind:?}", node_id(node)).into();
                        rows.insert(
                            id.clone(),
                            Row::Group {
                                kind,
                                count: items.len(),
                            },
                        );
                        let open = !self.is_collapsed_group(&id) || !filter.is_empty();
                        TreeItem::new(id, group_label(kind))
                            .expanded(open)
                            .children(items)
                    })
                    .collect(),
            )
        } else {
            Some(by_kind.into_iter().flat_map(|(_, v)| v).collect())
        }
    }

    fn is_collapsed_group(&self, id: &SharedString) -> bool {
        self.collapsed.contains(id)
    }

    fn inspect(&mut self, id: &SharedString, cx: &mut Context<Self>) {
        let row = self.rows.borrow().get(id).cloned();
        let ins = match row {
            Some(Row::Source(s)) => Inspection {
                title: s.config.name.clone(),
                subtitle: s.capabilities.language.display_name(),
                value: None,
                fields: source_fields(&s),
            },
            Some(Row::Object { object, .. }) => Inspection {
                title: object.name.clone(),
                subtitle: t(&format!(
                    "object-{}",
                    format!("{:?}", object.kind).to_lowercase()
                )),
                value: None,
                fields: std::iter::once((t("inspector-path"), path_text(&object.node)))
                    .chain(object.detail.clone().map(|d| (t("inspector-detail"), d)))
                    .collect(),
            },
            _ => return,
        };
        inspector::show(ins, cx);
    }

    /// The selected object, if any.
    pub fn selected_object(&self, cx: &App) -> Option<DbObject> {
        let id = self.tree.read(cx).selected_item()?.id.clone();
        match self.rows.borrow().get(&id)? {
            Row::Object { object, .. } => Some(object.clone()),
            _ => None,
        }
    }

    /// The data source of the selection (source row or any object below it).
    pub fn selected_source(&self, cx: &App) -> Option<DataSourceId> {
        let id = self.tree.read(cx).selected_item()?.id.clone();
        match self.rows.borrow().get(&id)? {
            Row::Source(s) => Some(s.config.id.clone()),
            Row::Object { object, .. } => Some(object.node.data_source.clone()),
            _ => None,
        }
    }
}

fn placeholder(node: &NodeRef, rows: &mut HashMap<SharedString, Row>) -> TreeItem {
    let id: SharedString = format!("pending:{}", node_id(node)).into();
    rows.insert(id.clone(), Row::Loading);
    TreeItem::new(id, "…").disabled(true)
}

fn path_text(node: &NodeRef) -> String {
    node.path
        .iter()
        .map(|(_, n)| n.as_str())
        .collect::<Vec<_>>()
        .join(" / ")
}

pub fn source_fields(s: &DataSourceInfo) -> Vec<(String, String)> {
    let c = &s.capabilities;
    let mut v = vec![
        (t("ds-driver"), s.config.driver.to_string()),
        (t("ds-language"), c.language.display_name()),
        (t("ds-model"), format!("{:?}", c.data_model)),
        (t("ds-access"), format!("{:?}", c.access)),
        (t("ds-transactions"), format!("{:?}", c.transactions)),
        (t("ds-read-only"), s.config.read_only.to_string()),
    ];
    v.extend(
        s.config
            .properties
            .iter()
            .map(|(k, val)| (k.clone(), val.clone())),
    );
    v
}

fn connect(id: DataSourceId, connect: bool, cx: &mut App) {
    let backend = AppState::backend(cx);
    let task = cx.background_executor().spawn(async move {
        let ds = backend.data_sources();
        if connect {
            ds.connect(&id).await.map_err(|e| format!("{id}: {e}"))
        } else {
            ds.disconnect(&id).await.map_err(|e| format!("{id}: {e}"))
        }
    });
    cx.spawn(async move |cx| {
        if let Err(e) = task.await {
            cx.update(|cx| output::log(output::Level::Error, e, cx));
        }
    })
    .detach();
}

/// Context menu entries for an object, by model and capabilities.
fn object_menu(
    menu: ferrodb_ui_kit::kit::menu::PopupMenu,
    object: DbObject,
    caps: Option<Capabilities>,
    panel: WeakEntity<ExplorerPanel>,
) -> ferrodb_ui_kit::kit::menu::PopupMenu {
    let kind = object.kind;
    let model = caps.as_ref().map(|c| c.data_model);
    let tabular_table = model == Some(DataModel::Tabular)
        && matches!(kind, ObjectKind::Table)
        && caps
            .as_ref()
            .is_some_and(|c| c.schema == SchemaKind::Strict);
    let joins = caps.as_ref().is_some_and(|c| c.joins);
    let o = object.clone();
    let mut menu = menu;
    if opens_data(kind) {
        let o = o.clone();
        menu = menu.item(
            PopupMenuItem::new(t("menu-open-data"))
                .on_click(move |_, w, cx| workspace::open_data(o.node.clone(), w, cx)),
        );
    }
    let o2 = o.clone();
    menu = menu.item(
        PopupMenuItem::new(t("action-new-console")).on_click(move |_, w, cx| {
            workspace::open_console(Some(o2.node.data_source.clone()), String::new(), w, cx)
        }),
    );
    let name = o.name.clone();
    menu = menu
        .item(
            PopupMenuItem::new(t("menu-copy-name")).on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(name.clone()))
            }),
        )
        .separator();
    let has_definition = match model {
        Some(DataModel::Tabular) => true,
        Some(DataModel::Document) => matches!(
            kind,
            ObjectKind::Database | ObjectKind::Collection | ObjectKind::View | ObjectKind::Index
        ),
        _ => false,
    };
    if has_definition {
        let o = o.clone();
        menu = menu.item(
            PopupMenuItem::new(t(if model == Some(DataModel::Document) {
                "menu-copy-script"
            } else {
                "menu-copy-ddl"
            }))
            .on_click(move |_, w, cx| workspace::show_ddl(o.node.clone(), w, cx)),
        );
    }
    let scripts: &[(ScriptKind, &str)] = match (model, kind) {
        (Some(DataModel::Tabular), ObjectKind::Table) => &[
            (ScriptKind::Select, "menu-generate-select"),
            (ScriptKind::Insert, "menu-generate-insert"),
            (ScriptKind::Update, "menu-generate-update"),
            (ScriptKind::Delete, "menu-generate-delete"),
            (ScriptKind::Drop, "menu-generate-drop"),
        ],
        (Some(DataModel::Tabular), ObjectKind::View | ObjectKind::MaterializedView) => &[
            (ScriptKind::Select, "menu-generate-select"),
            (ScriptKind::Drop, "menu-generate-drop"),
        ],
        (Some(DataModel::Tabular), ObjectKind::Procedure | ObjectKind::Function) => &[
            (ScriptKind::Call, "menu-generate-call"),
            (ScriptKind::Drop, "menu-generate-drop"),
        ],
        (
            Some(DataModel::Tabular),
            ObjectKind::Column
            | ObjectKind::Index
            | ObjectKind::ForeignKey
            | ObjectKind::PrimaryKey
            | ObjectKind::Trigger
            | ObjectKind::Sequence,
        ) => &[(ScriptKind::Drop, "menu-generate-drop")],
        (Some(DataModel::Document), ObjectKind::Collection) => &[
            (ScriptKind::Select, "menu-generate-find"),
            (ScriptKind::Insert, "menu-generate-insert"),
            (ScriptKind::Update, "menu-generate-update"),
            (ScriptKind::Delete, "menu-generate-delete"),
            (ScriptKind::Drop, "menu-generate-drop"),
        ],
        _ => &[],
    };
    for (script, label) in scripts {
        let (o, script) = (o.clone(), *script);
        menu = menu.item(
            PopupMenuItem::new(t(label))
                .on_click(move |_, w, cx| workspace::show_script(o.node.clone(), script, w, cx)),
        );
    }
    if model.is_some() && !matches!(kind, ObjectKind::Folder) {
        let o = o.clone();
        menu = menu.item(
            PopupMenuItem::new(t("action-quick-doc"))
                .on_click(move |_, _, cx| workspace::show_documentation(o.node.clone(), cx)),
        );
    }
    if tabular_table {
        let o = o.clone();
        menu = menu.item(
            PopupMenuItem::new(t("menu-modify-table")).on_click(move |_, w, cx| {
                workspace::open_table_editor(Some(o.node.clone()), w, cx)
            }),
        );
    }
    if model == Some(DataModel::Tabular)
        && joins
        && matches!(
            kind,
            ObjectKind::Schema | ObjectKind::Dataset | ObjectKind::Table
        )
    {
        let o = o.clone();
        menu = menu.item(
            PopupMenuItem::new(t("action-er-diagram"))
                .on_click(move |_, w, cx| workspace::open_er_diagram(o.node.clone(), w, cx)),
        );
    }
    if model == Some(DataModel::Document)
        && matches!(kind, ObjectKind::Collection | ObjectKind::Table)
    {
        let o = o.clone();
        menu = menu.item(
            PopupMenuItem::new(t("docs-infer-schema"))
                .on_click(move |_, w, cx| workspace::open_data(o.node.clone(), w, cx)),
        );
    }
    if opens_data(kind) {
        menu = menu
            .item(
                PopupMenuItem::new(t("action-export-data"))
                    .on_click(|_, w, cx| w.dispatch_action(Box::new(actions::ExportData), cx)),
            )
            .item(
                PopupMenuItem::new(t("action-import-data"))
                    .on_click(|_, w, cx| w.dispatch_action(Box::new(actions::ImportData), cx)),
            );
    }
    if caps
        .as_ref()
        .is_some_and(|c| c.explain != ExplainSupport::None)
        && opens_data(kind)
    {
        menu = menu.separator();
    }
    let node = o.node.clone();
    menu.separator().item(
        PopupMenuItem::new(t("action-refresh")).on_click(move |_, _, cx| {
            let _ = panel.update(cx, |p, cx| p.refresh(Some(node.clone()), cx));
        }),
    )
}

fn source_menu(
    menu: ferrodb_ui_kit::kit::menu::PopupMenu,
    s: DataSourceInfo,
    panel: WeakEntity<ExplorerPanel>,
) -> ferrodb_ui_kit::kit::menu::PopupMenu {
    let connected = s.state == ConnectionState::Connected;
    let id = s.config.id.clone();
    let (c_id, d_id, n_id, r_id, e_id, x_id) = (
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
        id.clone(),
        id,
    );
    menu.item(
        PopupMenuItem::new(t("menu-connect"))
            .disabled(connected)
            .on_click(move |_, _, cx| connect(c_id.clone(), true, cx)),
    )
    .item(
        PopupMenuItem::new(t("menu-disconnect"))
            .disabled(!connected)
            .on_click(move |_, _, cx| connect(d_id.clone(), false, cx)),
    )
    .separator()
    .item(
        PopupMenuItem::new(t("action-new-console")).on_click(move |_, window, cx| {
            workspace::open_console(Some(n_id.clone()), String::new(), window, cx)
        }),
    )
    .item(
        PopupMenuItem::new(t("action-refresh")).on_click(move |_, _, cx| {
            let node = NodeRef::root(r_id.clone());
            let _ = panel.update(cx, |p, cx| p.refresh(Some(node), cx));
        }),
    )
    .separator()
    .item(
        PopupMenuItem::new(t("action-edit-data-source")).on_click(move |_, window, cx| {
            crate::dialogs::data_source::open_edit(e_id.clone(), window, cx)
        }),
    )
    .item(
        PopupMenuItem::new(t("action-driver-manager"))
            .on_click(|_, w, cx| w.dispatch_action(Box::new(actions::OpenDriverManager), cx)),
    )
    .separator()
    .item(
        PopupMenuItem::new(t("menu-remove-data-source")).on_click(move |_, window, cx| {
            let id = x_id.clone();
            window.open_alert_dialog(cx, move |alert, _, _| {
                let id = id.clone();
                alert
                    .title(t("remove-data-source-title"))
                    .description(t_args(
                        "remove-data-source-description",
                        &[("id", id.to_string().into())],
                    ))
                    .confirm()
                    .on_ok(move |_, window, cx| {
                        let backend = AppState::backend(cx);
                        let id = id.clone();
                        let task = cx
                            .background_executor()
                            .spawn(async move { backend.data_sources().remove(&id).await });
                        window
                            .spawn(cx, async move |cx| {
                                if let Err(e) = task.await {
                                    let _ = cx.update(|w, cx| {
                                        w.push_notification(Notification::error(e.to_string()), cx)
                                    });
                                }
                            })
                            .detach();
                        true
                    })
            });
        }),
    )
}

panel_boilerplate!(ExplorerPanel);

impl BasePanel for ExplorerPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
    fn closable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for ExplorerPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-explorer")
    }
}

impl Render for ExplorerPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        let menu_rows = self.rows.clone();
        let panel = cx.entity().downgrade();
        let sources = self.sources.clone();
        let tool = |id: &'static str, icon: icons::Lucide, tip: &str, action: Box<dyn Action>| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(t(tip))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        let toolbar = h_flex()
            .gap_1()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(tool(
                "explorer-new-ds",
                icons::Lucide::DatabasePlus,
                "action-new-data-source",
                Box::new(actions::NewDataSource),
            ))
            .child(tool(
                "explorer-new-console",
                icons::Lucide::SquareTerminal,
                "action-new-console",
                Box::new(actions::NewConsole),
            ))
            .child(tool(
                "explorer-refresh",
                icons::Lucide::RefreshCw,
                "action-refresh",
                Box::new(actions::RefreshExplorer),
            ))
            .child(tool(
                "explorer-drivers",
                icons::Lucide::Plug,
                "action-driver-manager",
                Box::new(actions::OpenDriverManager),
            ));
        let filter = div().px_1().py_0p5().child(
            Input::new(&self.filter)
                .xsmall()
                .prefix(icons::Icon::from(icons::Lucide::Search).xsmall())
                .cleanable(true),
        );

        let open_panel = panel.clone();
        let tree = Tree::new(&self.tree, move |ix, entry, selected, _window, cx| {
            let id = entry.item().id.clone();
            let row = rows.borrow().get(&id).cloned();
            let theme = cx.theme();
            let muted = theme.muted_foreground;
            let indent = px(6. + 14. * entry.depth() as f32);
            let marker = |env: Environment| {
                div()
                    .w(px(3.))
                    .h(px(14.))
                    .rounded_sm()
                    .flex_shrink_0()
                    .when_some(environment_color(env, cx), |d, c| d.bg(c))
            };
            let mut open_target = None;
            let content = match row {
                Some(Row::Folder(name)) => h_flex()
                    .gap_1p5()
                    .child(
                        icons::object_icon(ObjectKind::Folder)
                            .small()
                            .text_color(muted),
                    )
                    .child(name),
                Some(Row::Source(s)) => {
                    let state_color = match s.state {
                        ConnectionState::Connected => Some(theme.success),
                        ConnectionState::Failed(_) => Some(theme.danger),
                        ConnectionState::Connecting => Some(theme.warning),
                        ConnectionState::Disconnected => None,
                    };
                    h_flex()
                        .gap_1p5()
                        .child(marker(s.config.environment))
                        .child(
                            icons::data_source_icon(s.capabilities.data_model)
                                .small()
                                .text_color(muted),
                        )
                        .child(s.config.name.clone())
                        .when(s.config.read_only, |d| {
                            d.child(
                                icons::Icon::from(icons::Lucide::Lock)
                                    .xsmall()
                                    .text_color(muted),
                            )
                        })
                        .when_some(state_color, |d, c| {
                            d.child(div().size(px(6.)).rounded_full().bg(c))
                        })
                }
                Some(Row::Group { kind, count }) => h_flex()
                    .gap_1p5()
                    .child(
                        icons::object_icon(ObjectKind::Folder)
                            .small()
                            .text_color(muted),
                    )
                    .child(group_label(kind))
                    .child(div().text_xs().text_color(muted).child(count.to_string())),
                Some(Row::Object { object, env }) => {
                    if opens_data(object.kind) {
                        open_target = Some(object.node.clone());
                    }
                    h_flex()
                        .gap_1p5()
                        .overflow_hidden()
                        .when(env == Environment::Production, |d| d.child(marker(env)))
                        .child(icons::object_icon(object.kind).small().text_color(muted))
                        .child(object.name.clone())
                        .when_some(object.detail.clone(), |d, x| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(x),
                            )
                        })
                }
                Some(Row::Loading) | None => {
                    h_flex().text_color(muted).child(entry.item().label.clone())
                }
                Some(Row::Failed(e)) => h_flex()
                    .gap_1()
                    .text_color(theme.danger)
                    .child(icons::Icon::from(icons::Lucide::CircleAlert).xsmall())
                    .child(e),
            };
            let panel = open_panel.clone();
            ListItem::new(ix)
                .selected(selected)
                .pl(indent)
                .when_some(open_target, |item, node| {
                    item.on_click(move |ev, window, cx| {
                        if ev.click_count() == 2 {
                            let _ = panel.upgrade();
                            workspace::open_data(node.clone(), window, cx);
                        }
                    })
                })
                .child(content)
        })
        .context_menu(move |_ix, entry, menu, _window, _cx| {
            match menu_rows.borrow().get(&entry.item().id).cloned() {
                Some(Row::Source(s)) => source_menu(menu, s, panel.clone()),
                Some(Row::Object { object, .. }) => {
                    let caps = sources
                        .iter()
                        .find(|s| s.config.id == object.node.data_source)
                        .map(|s| s.capabilities.clone());
                    object_menu(menu, object, caps, panel.clone())
                }
                _ => menu,
            }
        });

        v_flex()
            .id("explorer")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .text_sm()
            .on_action(
                cx.listener(|this, _: &actions::RefreshExplorer, _, cx| this.refresh(None, cx)),
            )
            .child(toolbar)
            .child(filter)
            .child(div().flex_1().min_h_0().child(tree.size_full()))
    }
}
