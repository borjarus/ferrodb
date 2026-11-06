//! Document browser (plan 2.5): tree / JSON / table views of BSON documents,
//! filter / sort / projection fields, type-faithful values, editing through
//! the operation preview, inferred schema and index panel.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use ferrodb_api::{
    Change, ChangeSet, DbObject, InferredSchema, NodeRef, ObjectKind, QueryLanguage, SampleOptions,
    bson::{Bson, Document},
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        h_flex,
        input::{Editor, EditorState, Input, InputEvent, InputState},
        list::ListItem,
        menu::PopupMenuItem,
        notification::Notification,
        tab::{Tab, TabBar},
        table::{Column, DataTable, TableDelegate, TableEvent, TableState},
        tree::{Tree, TreeItem, TreeState},
        v_flex,
    },
};

use super::{ResultData, ResultModel, ResultViewEvent, changes, format};
use crate::{
    AppState,
    i18n::{t, t_args},
    inspector::{self, Inspection},
    output,
};

/// Array elements shown under one node; the rest is summarized.
const MAX_ARRAY_CHILDREN: usize = 1000;
/// Documents rendered in the JSON view at once.
const MAX_JSON_DOCS: usize = 100;
/// Separator of label parts in tree items (`name␟value␟type`).
const SEP: char = '\u{1f}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocMode {
    Tree,
    Json,
    Table,
}

/// Decoded documents, cached by index (decoding is lazy).
#[derive(Clone, Default)]
struct DocCache(Rc<RefCell<HashMap<usize, Rc<Document>>>>);

impl DocCache {
    fn get(&self, model: &ResultModel, ix: usize) -> Option<Rc<Document>> {
        if let Some(d) = self.0.borrow().get(&ix) {
            return Some(d.clone());
        }
        let ResultData::Documents { docs, .. } = &model.data else {
            return None;
        };
        let d = Rc::new(format::document(docs.get(ix)?));
        let mut c = self.0.borrow_mut();
        if c.len() > 512 {
            c.clear();
        }
        c.insert(ix, d.clone());
        Some(d)
    }
}

/// Tree item id: `d<ix>` for a document, `d<ix>|<path>` for a field.
fn item_id(doc: usize, path: Option<&str>) -> SharedString {
    match path {
        Some(p) => format!("d{doc}|{p}").into(),
        None => format!("d{doc}").into(),
    }
}

pub fn parse_item_id(id: &str) -> Option<(usize, Option<String>)> {
    let rest = id.strip_prefix('d')?;
    match rest.split_once('|') {
        Some((d, p)) => Some((d.parse().ok()?, Some(p.to_owned()))),
        None => Some((rest.parse().ok()?, None)),
    }
}

fn field_items(doc: usize, prefix: &str, v: &Bson) -> Vec<TreeItem> {
    let child = |key: String, value: &Bson| {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let label = format!(
            "{key}{SEP}{}{SEP}{}",
            format::bson_short(value),
            format::bson_type(value)
        );
        TreeItem::new(item_id(doc, Some(&path)), label).children(field_items(doc, &path, value))
    };
    match v {
        Bson::Document(d) => d.iter().map(|(k, v)| child(k.clone(), v)).collect(),
        Bson::Array(a) => {
            let mut items: Vec<TreeItem> = a
                .iter()
                .take(MAX_ARRAY_CHILDREN)
                .enumerate()
                .map(|(i, v)| child(i.to_string(), v))
                .collect();
            if a.len() > MAX_ARRAY_CHILDREN {
                let more = a.len() - MAX_ARRAY_CHILDREN;
                items.push(
                    TreeItem::new(
                        format!("d{doc}|{prefix}.…").as_str().to_owned(),
                        format!(
                            "…{SEP}{}{SEP}",
                            t_args("docs-more-items", &[("count", more.into())])
                        ),
                    )
                    .disabled(true),
                );
            }
            items
        }
        _ => Vec::new(),
    }
}

fn doc_item(ix: usize, doc: &Document) -> TreeItem {
    let id = doc
        .get("_id")
        .map(format::bson_short)
        .unwrap_or_else(|| format!("#{}", ix + 1));
    let label = format!(
        "{id}{SEP}{}{SEP}Document",
        t_args("docs-fields", &[("count", doc.len().into())])
    );
    TreeItem::new(item_id(ix, None), label).children(field_items(
        ix,
        "",
        &Bson::Document(doc.clone()),
    ))
}

// ------------------------------------------------------------------ table mode

pub struct DocTableDelegate {
    model: Entity<ResultModel>,
    cache: DocCache,
    fields: Vec<String>,
}

impl DocTableDelegate {
    fn refresh_fields(&mut self, cx: &App) {
        // Columns: top-level fields of the first documents, in order of appearance.
        let m = self.model.read(cx);
        let mut fields: Vec<String> = Vec::new();
        for i in 0..m.data.len().min(200) {
            if let Some(d) = self.cache.get(m, i) {
                for k in d.keys() {
                    if !fields.contains(k) {
                        fields.push(k.clone());
                    }
                }
            }
        }
        self.fields = fields;
    }
}

impl TableDelegate for DocTableDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.fields.len()
    }
    fn rows_count(&self, cx: &App) -> usize {
        self.model.read(cx).data.len()
    }
    fn column(&self, col_ix: usize, _: &App) -> Column {
        let f = &self.fields[col_ix];
        Column::new(f.clone(), f.clone()).width(px(150.))
    }
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let m = self.model.read(cx);
        let v = self
            .cache
            .get(m, row_ix)
            .and_then(|d| d.get(&self.fields[col_ix]).map(format::bson_short));
        let muted = cx.theme().muted_foreground;
        div()
            .overflow_hidden()
            .whitespace_nowrap()
            .map(|d| match v {
                Some(v) => d.child(v),
                None => d.text_color(muted).child("—"),
            })
    }
    fn has_more(&self, cx: &App) -> bool {
        let m = self.model.read(cx);
        m.has_more && !m.fetching
    }
    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        ResultModel::fetch_more(&self.model, false, cx);
    }
}

// ------------------------------------------------------------------ view

pub struct DocumentView {
    model: Entity<ResultModel>,
    pub mode: DocMode,
    cache: DocCache,
    tree: Entity<TreeState>,
    json: Entity<EditorState>,
    table: Entity<TableState<DocTableDelegate>>,
    filter: Entity<InputState>,
    sort: Entity<InputState>,
    projection: Entity<InputState>,
    /// Documents already turned into tree items.
    built: usize,
    items: Vec<TreeItem>,
    /// (document index, field path) of the selection.
    selected: Option<(usize, Option<String>)>,
    pub pending: Vec<Change>,
    pub schema_panel: bool,
    pub schema: Option<InferredSchema>,
    schema_loading: bool,
    indexes: Vec<DbObject>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ResultViewEvent> for DocumentView {}

impl DocumentView {
    pub fn new(model: Entity<ResultModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let cache = DocCache::default();
        let tree = cx.new(|cx| TreeState::new(cx));
        let json = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("javascript")
                .line_number(true)
        });
        json.update(cx, |e, cx| e.set_readonly(true, cx));
        let delegate = DocTableDelegate {
            model: model.clone(),
            cache: cache.clone(),
            fields: Vec::new(),
        };
        let table = cx.new(|cx| TableState::new(delegate, window, cx).row_selectable(true));
        let mql = model.read(cx).capabilities.language == QueryLanguage::Mql;
        let input = |ph: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let filter = input(
            if mql {
                "{ field: value }".into()
            } else {
                "WHERE …".into()
            },
            window,
            cx,
        );
        let sort = input(
            if mql {
                "{ field: 1 }".into()
            } else {
                "ORDER BY …".into()
            },
            window,
            cx,
        );
        let projection = input("{ field: 1 }".into(), window, cx);
        let subs = vec![
            cx.observe(&model, |this, _, cx| this.sync(cx)),
            cx.observe(&tree, |this, tree, cx| {
                let sel = tree
                    .read(cx)
                    .selected_item()
                    .and_then(|i| parse_item_id(&i.id));
                if sel.is_some() && sel != this.selected {
                    this.selected = sel;
                    this.publish_selection(cx);
                }
            }),
            cx.subscribe(&table, |this, _, ev: &TableEvent, cx| {
                if let TableEvent::SelectRow(r) = ev {
                    this.selected = Some((*r, None));
                    this.publish_selection(cx);
                }
            }),
            cx.subscribe_in(&filter, window, Self::on_query_input),
            cx.subscribe_in(&sort, window, Self::on_query_input),
            cx.subscribe_in(&projection, window, Self::on_query_input),
        ];
        let mut this = Self {
            model,
            mode: DocMode::Tree,
            cache,
            tree,
            json,
            table,
            filter,
            sort,
            projection,
            built: 0,
            items: Vec::new(),
            selected: None,
            pending: Vec::new(),
            schema_panel: false,
            schema: None,
            schema_loading: false,
            indexes: Vec::new(),
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.sync(cx);
        this
    }

    pub fn model(&self) -> &Entity<ResultModel> {
        &self.model
    }

    pub fn tree(&self) -> &Entity<TreeState> {
        &self.tree
    }

    /// Turns newly arrived documents into tree items.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let len = self.model.read(cx).data.len();
        if len < self.built {
            self.built = 0;
            self.items.clear();
        }
        if len > self.built {
            let m = self.model.read(cx);
            for i in self.built..len {
                if let Some(d) = self.cache.get(m, i) {
                    self.items.push(doc_item(i, &d));
                }
            }
            self.built = len;
            let items = self.items.clone();
            self.tree.update(cx, |t, cx| t.set_items(items, cx));
            self.table.update(cx, |t, cx| {
                if t.delegate().fields.is_empty() {
                    t.delegate_mut().refresh_fields(cx);
                }
                t.refresh(cx);
            });
        }
        cx.notify();
    }

    fn container(&self, cx: &App) -> String {
        match &self.model.read(cx).data {
            ResultData::Documents { container, .. } => {
                container.clone().unwrap_or_else(|| "collection".into())
            }
            _ => String::new(),
        }
    }

    pub fn set_mode(&mut self, mode: DocMode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        if mode == DocMode::Json {
            let text = self.json_text(cx);
            self.json.update(cx, |e, cx| e.set_value(text, window, cx));
        }
        cx.notify();
    }

    /// Selected document, or the first `MAX_JSON_DOCS` documents as an array.
    pub fn json_text(&self, cx: &App) -> String {
        let m = self.model.read(cx);
        if let Some((ix, _)) = &self.selected
            && let Some(d) = self.cache.get(m, *ix)
        {
            return format::document_json(&d);
        }
        let docs: Vec<String> = (0..m.data.len().min(MAX_JSON_DOCS))
            .filter_map(|i| self.cache.get(m, i))
            .map(|d| format::document_json(&d))
            .collect();
        format!("[\n{}\n]", docs.join(",\n"))
    }

    fn publish_selection(&mut self, cx: &mut Context<Self>) {
        let Some((ix, path)) = self.selected.clone() else {
            return;
        };
        let m = self.model.read(cx);
        let Some(doc) = self.cache.get(m, ix) else {
            return;
        };
        let (title, value, ty) = match &path {
            Some(p) => {
                let v = format::bson_at(&doc, p).cloned().unwrap_or(Bson::Null);
                let text = match &v {
                    Bson::Document(d) => format::document_json(d),
                    other => format::value_json(other),
                };
                (p.clone(), text, format::bson_type(&v).to_owned())
            }
            None => (
                doc.get("_id").map(format::bson_short).unwrap_or_default(),
                format::document_json(&doc),
                "Document".to_owned(),
            ),
        };
        let fields = doc
            .iter()
            .map(|(k, v)| (k.clone(), format::bson_short(v)))
            .collect();
        inspector::show(
            Inspection {
                title,
                subtitle: ty,
                value: Some(value),
                fields,
            },
            cx,
        );
        cx.notify();
    }

    fn on_query_input(
        &mut self,
        _: &Entity<InputState>,
        ev: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { .. } = ev {
            let q = self.query_text(cx);
            cx.emit(ResultViewEvent::Rerun(q));
        }
    }

    /// Query built from the filter / sort / projection fields, in the source language.
    pub fn query_text(&self, cx: &App) -> String {
        let c = self.container(cx);
        let (f, s, p) = (
            self.filter.read(cx).value().trim().to_string(),
            self.sort.read(cx).value().trim().to_string(),
            self.projection.read(cx).value().trim().to_string(),
        );
        build_query(&self.model.read(cx).capabilities.language, &c, &f, &s, &p)
    }

    /// Opens the value editor for the selected field.
    pub fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, Some(path))) = self.selected.clone() else {
            return;
        };
        if !self.check_editable(window, cx) {
            return;
        }
        let m = self.model.read(cx);
        let Some(doc) = self.cache.get(m, ix) else {
            return;
        };
        let current = format::bson_at(&doc, &path)
            .map(format::value_json)
            .unwrap_or_else(|| "null".into());
        let input = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("javascript")
                .line_number(false)
                .default_value(current)
        });
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let ok = {
                let (view, input, path) = (view.clone(), input.clone(), path.clone());
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let text = input.read(cx).value().to_string();
                    match format::parse_value_json(&text) {
                        Ok(v) => {
                            window.close_dialog(cx);
                            let _ = view.update(cx, |this, cx| {
                                this.pending.push(Change::SetField {
                                    document: ix,
                                    path: path.clone(),
                                    value: format::value_json(&v),
                                });
                                cx.notify();
                            });
                        }
                        Err(e) => window.push_notification(Notification::error(e), cx),
                    }
                }
            };
            dialog
                .title(t_args("docs-edit-field", &[("path", path.clone().into())]))
                .w(px(520.))
                .child(div().h(px(180.)).child(Editor::new(&input).size_full()))
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("doc-edit-cancel")
                                .label(t("button-cancel"))
                                .on_click(|_, w, cx| w.close_dialog(cx)),
                        )
                        .child(
                            Button::new("doc-edit-ok")
                                .primary()
                                .label(t("button-ok"))
                                .on_click(ok),
                        ),
                )
        });
    }

    /// Stages a field change (used by the edit dialog and UI tests).
    pub fn stage_field(&mut self, doc: usize, path: &str, value: Bson, cx: &mut Context<Self>) {
        self.pending.push(Change::SetField {
            document: doc,
            path: path.into(),
            value: format::value_json(&value),
        });
        cx.notify();
    }

    pub fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((ix, _)) = self.selected else { return };
        if !self.check_editable(window, cx) {
            return;
        }
        self.pending.push(Change::DeleteDocument { document: ix });
        cx.notify();
    }

    fn check_editable(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.model.read(cx).editable() {
            return true;
        }
        window.push_notification(Notification::warning(t("grid-read-only")), cx);
        false
    }

    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = ChangeSet {
            changes: self.pending.clone(),
        };
        let view = cx.entity().downgrade();
        changes::review_and_apply(
            &self.model,
            changes,
            move |_, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.pending.clear();
                    cx.emit(ResultViewEvent::Applied);
                    cx.notify();
                });
            },
            window,
            cx,
        );
    }

    /// Samples the container and fills the schema panel (plan 5.7).
    pub fn infer_schema(&mut self, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        let node =
            NodeRef::root(m.data_source.clone()).child(ObjectKind::Collection, self.container(cx));
        let backend = AppState::backend(cx);
        self.schema_panel = true;
        self.schema_loading = true;
        cx.notify();
        let task = cx.background_executor().spawn({
            let backend = backend.clone();
            let node = node.clone();
            async move {
                let schema = backend
                    .metadata()
                    .infer_schema(node.clone(), SampleOptions::default())
                    .await;
                let indexes = backend.metadata().children(node).await;
                (schema, indexes)
            }
        });
        cx.spawn(async move |this, cx| {
            let (schema, indexes) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.schema_loading = false;
                match schema {
                    Ok(s) => this.schema = Some(s),
                    Err(e) => output::log(output::Level::Error, e.to_string(), cx),
                }
                this.indexes = indexes
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|o| o.kind == ObjectKind::Index)
                    .collect();
                cx.notify();
            });
        })
        .detach();
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let mql = m.capabilities.language == QueryLanguage::Mql;
        let editable = m.editable();
        let pending = self.pending.len();
        let mode_ix = match self.mode {
            DocMode::Tree => 0,
            DocMode::Json => 1,
            DocMode::Table => 2,
        };
        h_flex()
            .gap_1()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                TabBar::new("doc-mode")
                    .xsmall()
                    .selected_index(mode_ix)
                    .child(Tab::new().label(t("docs-mode-tree")))
                    .child(Tab::new().label("JSON"))
                    .child(Tab::new().label(t("docs-mode-table")))
                    .on_click(cx.listener(|this, ix: &usize, w, cx| {
                        let mode = [DocMode::Tree, DocMode::Json, DocMode::Table][*ix];
                        this.set_mode(mode, w, cx)
                    })),
            )
            .child(div().w(px(220.)).child(Input::new(&self.filter).xsmall()))
            .child(div().w(px(140.)).child(Input::new(&self.sort).xsmall()))
            .when(mql, |d| {
                d.child(
                    div()
                        .w(px(140.))
                        .child(Input::new(&self.projection).xsmall()),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("doc-edit")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Pencil)
                    .tooltip(t("docs-edit"))
                    .disabled(!editable)
                    .on_click(cx.listener(|this, _, w, cx| this.edit_selected(w, cx))),
            )
            .child(
                Button::new("doc-delete")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Trash)
                    .tooltip(t("docs-delete"))
                    .disabled(!editable)
                    .on_click(cx.listener(|this, _, w, cx| this.delete_selected(w, cx))),
            )
            .child(
                Button::new("doc-submit")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Check)
                    .tooltip(t("action-submit-changes"))
                    .disabled(pending == 0)
                    .on_click(cx.listener(|this, _, w, cx| this.submit(w, cx))),
            )
            .child(
                Button::new("doc-schema-toggle")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::ListTree)
                    .tooltip(t("docs-infer-schema"))
                    .selected(self.schema_panel)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.schema_panel {
                            this.schema_panel = false;
                            cx.notify();
                        } else {
                            this.infer_schema(cx)
                        }
                    })),
            )
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let accent = cx.theme().primary;
        let view = cx.entity().downgrade();
        let menu_view = view.clone();
        Tree::new(&self.tree, move |ix, entry, selected, _, _| {
            let label = entry.item().label.clone();
            let mut parts = label.split(SEP);
            let (key, value, ty) = (
                parts.next().unwrap_or_default().to_owned(),
                parts.next().unwrap_or_default().to_owned(),
                parts.next().unwrap_or_default().to_owned(),
            );
            let view = view.clone();
            ListItem::new(ix)
                .selected(selected)
                .pl(px(8. + 16. * entry.depth() as f32))
                .on_click(move |ev, window, cx| {
                    if ev.click_count() == 2 {
                        let _ = view.update(cx, |v, cx| v.edit_selected(window, cx));
                    }
                })
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .overflow_hidden()
                        .child(div().text_color(accent).flex_shrink_0().child(key))
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(value),
                        )
                        .child(div().text_xs().text_color(muted).child(ty)),
                )
        })
        .context_menu(move |_, entry, menu, _, _| {
            let Some((doc, path)) = parse_item_id(&entry.item().id) else {
                return menu;
            };
            let v = menu_view.clone();
            let v2 = menu_view.clone();
            let v3 = menu_view.clone();
            let p2 = path.clone();
            menu.item(
                PopupMenuItem::new(t("docs-edit"))
                    .disabled(path.is_none())
                    .on_click(move |_, w, cx| {
                        let _ = v.update(cx, |this, cx| {
                            this.selected = Some((doc, p2.clone()));
                            this.edit_selected(w, cx)
                        });
                    }),
            )
            .item(
                PopupMenuItem::new(t("docs-copy-json")).on_click(move |_, _, cx| {
                    let _ = v2.update(cx, |this, cx| {
                        this.selected = Some((doc, None));
                        let text = this.json_text(cx);
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    });
                }),
            )
            .separator()
            .item(
                PopupMenuItem::new(t("docs-delete")).on_click(move |_, w, cx| {
                    let _ = v3.update(cx, |this, cx| {
                        this.selected = Some((doc, None));
                        this.delete_selected(w, cx)
                    });
                }),
            )
        })
        .size_full()
    }

    fn render_schema(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("doc-schema")
            .test_support()
            .w(px(280.))
            .h_full()
            .overflow_y_scroll()
            .border_l_1()
            .border_color(theme.border)
            .p_2()
            .gap_1()
            .text_sm()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("docs-schema")),
            )
            .when(self.schema_loading, |d| d.child(t("loading")))
            .when_some(self.schema.as_ref(), |d, s| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t_args("docs-sampled", &[("count", s.sampled.into())])),
                )
                .children(s.fields.iter().map(|f| {
                    let types = f
                        .types
                        .iter()
                        .map(|(t, p)| format!("{t} {:.0}%", p * 100.0))
                        .collect::<Vec<_>>()
                        .join(", ");
                    v_flex()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(f.path.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(types),
                                ),
                        )
                        .child(
                            div()
                                .h(px(3.))
                                .w_full()
                                .bg(theme.muted)
                                .child(div().h_full().w(relative(f.presence)).bg(theme.primary)),
                        )
                }))
            })
            .when(!self.indexes.is_empty(), |d| {
                d.child(
                    div()
                        .pt_2()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t("docs-indexes")),
                )
                .children(self.indexes.iter().map(|i| {
                    h_flex()
                        .gap_1()
                        .child(icons::object_icon(ObjectKind::Index).xsmall())
                        .child(i.name.clone())
                        .when_some(i.detail.clone(), |d, x| {
                            d.child(div().text_xs().text_color(theme.muted_foreground).child(x))
                        })
                }))
            })
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let (loaded, has_more, fetching) = (m.data.len(), m.has_more, m.fetching);
        h_flex()
            .id("doc-footer")
            .test_support()
            .gap_3()
            .px_2()
            .py_0p5()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(t_args(
                "docs-count",
                &[
                    ("count", loaded.into()),
                    ("more", if has_more { "yes" } else { "no" }.into()),
                ],
            ))
            .when(!self.pending.is_empty(), |d| {
                d.child(div().text_color(cx.theme().warning).child(t_args(
                    "grid-pending",
                    &[("count", self.pending.len().into())],
                )))
            })
            .child(div().flex_1())
            .child(
                Button::new("doc-load-more")
                    .ghost()
                    .xsmall()
                    .label(t("grid-load-more"))
                    .loading(fetching)
                    .disabled(!has_more)
                    .on_click(cx.listener(|this, _, _, cx| {
                        ResultModel::fetch_more(&this.model, false, cx)
                    })),
            )
    }
}

/// Query text for the document filter fields.
pub fn build_query(
    lang: &QueryLanguage,
    container: &str,
    filter: &str,
    sort: &str,
    projection: &str,
) -> String {
    match lang {
        QueryLanguage::Mql => {
            let f = if filter.is_empty() { "{}" } else { filter };
            let mut q = if projection.is_empty() {
                format!("db.{container}.find({f})")
            } else {
                format!("db.{container}.find({f}, {projection})")
            };
            if !sort.is_empty() {
                q.push_str(&format!(".sort({sort})"));
            }
            q
        }
        QueryLanguage::PartiQl => {
            let mut q = format!("SELECT * FROM \"{container}\"");
            if !filter.is_empty() {
                q.push_str(&format!(" WHERE {filter}"));
            }
            q
        }
        _ => {
            let mut q = format!("SELECT * FROM {container}");
            if !filter.is_empty() {
                q.push_str(&format!(" WHERE {filter}"));
            }
            if !sort.is_empty() {
                q.push_str(&format!(" ORDER BY {sort}"));
            }
            q
        }
    }
}

impl Focusable for DocumentView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DocumentView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.mode {
            DocMode::Tree => self.render_tree(cx).into_any_element(),
            DocMode::Json => Editor::new(&self.json).size_full().into_any_element(),
            DocMode::Table => DataTable::new(&self.table)
                .stripe(true)
                .xsmall()
                .into_any_element(),
        };
        v_flex()
            .id("document-view")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .child(self.render_toolbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_1().h_full().min_w_0().child(body))
                    .when(self.schema_panel, |d| d.child(self.render_schema(cx))),
            )
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn ids_roundtrip() {
        assert_eq!(parse_item_id("d12"), Some((12, None)));
        assert_eq!(
            parse_item_id("d3|address.city"),
            Some((3, Some("address.city".into())))
        );
        assert_eq!(parse_item_id("x"), None);
    }

    #[::core::prelude::v1::test]
    fn queries_per_language() {
        assert_eq!(
            build_query(&QueryLanguage::Mql, "c", "{a: 1}", "{b: -1}", "{a: 1}"),
            "db.c.find({a: 1}, {a: 1}).sort({b: -1})"
        );
        assert_eq!(
            build_query(&QueryLanguage::PartiQl, "Orders", "pk = 'x'", "", ""),
            "SELECT * FROM \"Orders\" WHERE pk = 'x'"
        );
        assert_eq!(
            build_query(&QueryLanguage::SqlPlusPlus, "airline", "", "name", ""),
            "SELECT * FROM airline ORDER BY name"
        );
    }

    #[::core::prelude::v1::test]
    fn big_arrays_are_capped() {
        let d = ferrodb_api::bson::doc! { "a": (0..1500).collect::<Vec<i32>>() };
        let item = doc_item(0, &d);
        let arr = &item.children[0];
        assert_eq!(arr.children.len(), MAX_ARRAY_CHILDREN + 1);
    }
}
