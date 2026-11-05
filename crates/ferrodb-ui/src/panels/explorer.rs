//! Database Explorer (phase 1: data sources grouped into folders, with
//! environment colors and connection state; object trees arrive in phase 2).

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use ferrodb_api::{ConnectionState, DataModel, DataSourceId, DataSourceInfo, Environment};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex,
        list::ListItem,
        menu::PopupMenuItem,
        tree::{Tree, TreeItem, TreeState},
        v_flex,
    },
    theme::environment_color,
};

use super::panel_boilerplate;
use crate::{AppState, actions, i18n::t, output, workspace};

pub const NAME: &str = "Explorer";

#[derive(Clone)]
enum Row {
    Folder(String),
    Source {
        id: DataSourceId,
        name: String,
        model: DataModel,
        env: Environment,
        state: ConnectionState,
    },
}

pub struct ExplorerPanel {
    focus: FocusHandle,
    tree: Entity<TreeState>,
    rows: Rc<RefCell<HashMap<SharedString, Row>>>,
    _events: Task<()>,
}

impl ExplorerPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let rx = AppState::backend(cx).data_sources().subscribe();
        let events = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            focus: cx.focus_handle(),
            tree,
            rows: Rc::default(),
            _events: events,
        };
        this.reload(cx);
        this
    }

    /// Rebuilds tree items from the backend, keeping expansion state.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let sources = AppState::backend(cx).data_sources().list();
        let (items, rows) = build_items(&sources);
        *self.rows.borrow_mut() = rows;
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        cx.notify();
    }
}

fn build_items(sources: &[DataSourceInfo]) -> (Vec<TreeItem>, HashMap<SharedString, Row>) {
    let mut rows = HashMap::new();
    let mut folders: Vec<(String, Vec<TreeItem>)> = Vec::new();
    let mut top = Vec::new();
    for s in sources {
        let id: SharedString = format!("ds:{}", s.config.id).into();
        rows.insert(
            id.clone(),
            Row::Source {
                id: s.config.id.clone(),
                name: s.config.name.clone(),
                model: s.capabilities.data_model,
                env: s.config.environment,
                state: s.state.clone(),
            },
        );
        let item = TreeItem::new(id, s.config.name.clone());
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
            TreeItem::new(id, name).expanded(true).children(children)
        })
        .collect();
    items.extend(top);
    (items, rows)
}

fn connect(id: DataSourceId, connect: bool, cx: &mut App) {
    let backend = AppState::backend(cx);
    cx.spawn(async move |cx| {
        let ds = backend.data_sources();
        let result = if connect {
            ds.connect(&id).await
        } else {
            ds.disconnect(&id).await
        };
        if let Err(e) = result {
            cx.update(|cx| output::log(output::Level::Error, format!("{id}: {e}"), cx));
        }
    })
    .detach();
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
        let toolbar = h_flex()
            .gap_1()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("explorer-new-ds")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::DatabasePlus)
                    .tooltip(t("action-new-data-source"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(actions::NewDataSource), cx)
                    }),
            )
            .child(
                Button::new("explorer-new-console")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::SquareTerminal)
                    .tooltip(t("action-new-console"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(actions::NewConsole), cx)
                    }),
            );

        let tree = Tree::new(&self.tree, move |ix, entry, selected, _window, cx| {
            let id = entry.item().id.clone();
            let row = rows.borrow().get(&id).cloned();
            let theme = cx.theme();
            let indent = px(6. + 14. * entry.depth() as f32);
            let content = match row {
                Some(Row::Folder(name)) => h_flex()
                    .gap_1p5()
                    .child(
                        icons::object_icon(ferrodb_api::ObjectKind::Folder)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(name),
                Some(Row::Source {
                    name,
                    model,
                    env,
                    state,
                    ..
                }) => {
                    let marker = environment_color(env, cx);
                    let state_color = match state {
                        ConnectionState::Connected => Some(theme.success),
                        ConnectionState::Failed(_) => Some(theme.danger),
                        ConnectionState::Connecting => Some(theme.warning),
                        ConnectionState::Disconnected => None,
                    };
                    h_flex()
                        .gap_1p5()
                        .child(
                            div()
                                .w(px(3.))
                                .h(px(14.))
                                .rounded_sm()
                                .when_some(marker, |d, c| d.bg(c)),
                        )
                        .child(
                            icons::data_source_icon(model)
                                .small()
                                .text_color(theme.muted_foreground),
                        )
                        .child(name)
                        .when_some(state_color, |d, c| {
                            d.child(div().size(px(6.)).rounded_full().bg(c))
                        })
                }
                None => h_flex().child(entry.item().label.clone()),
            };
            ListItem::new(ix)
                .selected(selected)
                .pl(indent)
                .child(content)
        })
        .context_menu(move |_ix, entry, menu, _window, _cx| {
            let Some(Row::Source { id, state, .. }) =
                menu_rows.borrow().get(&entry.item().id).cloned()
            else {
                return menu;
            };
            let connected = state == ConnectionState::Connected;
            let (c_id, d_id, n_id) = (id.clone(), id.clone(), id);
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
        });

        v_flex()
            .id("explorer")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .text_sm()
            .child(toolbar)
            .child(div().flex_1().child(tree.size_full()))
    }
}
