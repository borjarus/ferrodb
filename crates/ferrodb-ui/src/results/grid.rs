//! Result grid for tabular results (plan 2.4).
//!
//! `DataTable` virtualizes rows and columns; cells are formatted from the
//! Arrow batches on demand. Sorting and searching are local (loaded rows);
//! the WHERE / ORDER BY fields re-run the statement on the server. Edits are
//! collected as pending changes and applied through the operation preview.

use std::collections::{BTreeMap, BTreeSet};

use ferrodb_api::{Change, ChangeSet, QueryLanguage};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        menu::PopupMenuItem,
        notification::Notification,
        table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState},
        v_flex,
    },
};

use super::{
    ResultModel, ResultViewEvent, TableData, changes,
    format::{self, NULL, ValueKind},
};
use crate::{
    actions,
    i18n::{t, t_args},
    inspector::{self, Inspection},
};

pub use crate::actions::GRID_CONTEXT;

pub struct GridDelegate {
    model: Entity<ResultModel>,
    view: WeakEntity<GridView>,
    columns: Vec<Column>,
    kinds: Vec<ValueKind>,
    /// View row → model row, when sorted or searched locally.
    order: Option<Vec<usize>>,
    /// Pending cell edits: (model row, col) → new text (`None` = NULL).
    pub pending: BTreeMap<(usize, usize), Option<String>>,
    pub deleted: BTreeSet<usize>,
    pub inserted: Vec<Vec<Option<String>>>,
    /// Cell being edited inline.
    editing: Option<((usize, usize), Entity<InputState>)>,
}

impl GridDelegate {
    fn new(model: Entity<ResultModel>, view: WeakEntity<GridView>, cx: &App) -> Self {
        let mut this = Self {
            model,
            view,
            columns: Vec::new(),
            kinds: Vec::new(),
            order: None,
            pending: BTreeMap::new(),
            deleted: BTreeSet::new(),
            inserted: Vec::new(),
            editing: None,
        };
        this.rebuild_columns(cx);
        this
    }

    fn rebuild_columns(&mut self, cx: &App) {
        let Some(t) = table(&self.model, cx) else {
            return;
        };
        self.columns = t
            .schema
            .fields()
            .iter()
            .map(|f| {
                let c = Column::new(f.name().clone(), f.name().clone())
                    .width(px(140.))
                    .sortable();
                match format::value_kind(f.data_type()) {
                    ValueKind::Integer | ValueKind::Float => c.text_right(),
                    _ => c,
                }
            })
            .collect();
        self.kinds = t
            .schema
            .fields()
            .iter()
            .map(|f| format::value_kind(f.data_type()))
            .collect();
    }

    /// Model row shown at view row `row`.
    pub fn model_row(&self, row: usize) -> usize {
        match &self.order {
            Some(o) => o.get(row).copied().unwrap_or(row),
            None => row,
        }
    }

    /// Current text of a cell, including pending edits (`None` = NULL).
    pub fn cell_text(&self, row: usize, col: usize, cx: &App) -> Option<String> {
        if let Some(p) = self.pending.get(&(row, col)) {
            return p.clone();
        }
        table(&self.model, cx).and_then(|t| t.cell(row, col))
    }

    pub fn change_set(&self) -> ChangeSet {
        let mut changes: Vec<Change> = self
            .pending
            .iter()
            .filter(|((r, _), _)| !self.deleted.contains(r))
            .map(|((row, col), v)| Change::SetCell {
                row: *row,
                column: *col,
                // Already validated when the edit was committed.
                value: format::encode_cell(self.kinds[*col], v.as_deref())
                    .unwrap_or_else(|_| "null".into()),
            })
            .collect();
        changes.extend(self.inserted.iter().map(|values| {
            Change::InsertRow {
                values: values
                    .iter()
                    .enumerate()
                    .map(|(c, v)| {
                        v.as_ref()
                            .and_then(|v| format::encode_cell(self.kinds[c], Some(v)).ok())
                    })
                    .collect(),
            }
        }));
        changes.extend(
            self.deleted
                .iter()
                .map(|row| Change::DeleteRow { row: *row }),
        );
        ChangeSet { changes }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len() + self.deleted.len() + self.inserted.len()
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
        self.deleted.clear();
        self.inserted.clear();
        self.editing = None;
    }

    /// Recomputes `order` from the local search and sort.
    fn apply_local(&mut self, search: &str, sort: Option<(usize, ColumnSort)>, cx: &App) {
        let Some(t) = table(&self.model, cx) else {
            return;
        };
        let needle = search.trim().to_lowercase();
        if needle.is_empty() && sort.is_none_or(|(_, s)| s == ColumnSort::Default) {
            self.order = None;
            return;
        }
        let cols = t.schema.fields().len();
        let mut rows: Vec<usize> = (0..t.rows)
            .filter(|&r| {
                needle.is_empty()
                    || (0..cols).any(|c| {
                        t.cell(r, c)
                            .is_some_and(|v| v.to_lowercase().contains(&needle))
                    })
            })
            .collect();
        if let Some((col, dir)) = sort.filter(|(_, s)| *s != ColumnSort::Default) {
            let numeric = matches!(self.kinds[col], ValueKind::Integer | ValueKind::Float);
            if numeric {
                rows.sort_by(|a, b| {
                    let (x, y) = (t.number(*a, col), t.number(*b, col));
                    x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                });
            } else {
                rows.sort_by_cached_key(|r| t.cell(*r, col));
            }
            if dir == ColumnSort::Descending {
                rows.reverse();
            }
        }
        self.order = Some(rows);
    }
}

fn table<'a>(model: &'a Entity<ResultModel>, cx: &'a App) -> Option<&'a TableData> {
    match &model.read(cx).data {
        super::ResultData::Table(t) => Some(t),
        _ => None,
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, cx: &App) -> usize {
        match &self.order {
            Some(o) => o.len(),
            None => self.model.read(cx).data.len(),
        }
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let ty = table(&self.model, cx)
            .and_then(|t| {
                t.schema
                    .fields()
                    .get(col_ix)
                    .map(|f| format::type_label(f.data_type()))
            })
            .unwrap_or_default();
        h_flex()
            .gap_1()
            .overflow_hidden()
            .child(self.columns[col_ix].name.clone())
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(ty),
            )
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let search = self
            .view
            .upgrade()
            .map(|v| v.read(cx).search.read(cx).value().to_string())
            .unwrap_or_default();
        if let Some(v) = self.view.upgrade() {
            v.update(cx, |v, _| v.sort = Some((col_ix, sort)));
        }
        self.apply_local(&search, Some((col_ix, sort)), cx);
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let row = self.model_row(row_ix);
        if let Some(((r, c), input)) = &self.editing
            && *r == row
            && *c == col_ix
        {
            return div()
                .size_full()
                .child(Input::new(input).xsmall().appearance(false))
                .into_any_element();
        }
        let theme = cx.theme();
        let edited = self.pending.contains_key(&(row, col_ix));
        let deleted = self.deleted.contains(&row);
        let value = self.cell_text(row, col_ix, cx);
        div()
            .size_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .when(edited, |d| d.bg(theme.warning.opacity(0.18)))
            .when(deleted, |d| d.bg(theme.danger.opacity(0.15)).line_through())
            .map(|d| match value {
                Some(v) => d.child(v.lines().next().unwrap_or_default().to_owned()),
                None => d.italic().text_color(theme.muted_foreground).child(NULL),
            })
            .into_any_element()
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, cx: &App) -> String {
        let row = self.model_row(row_ix);
        GridDelegate::cell_text(self, row, col_ix, cx).unwrap_or_default()
    }

    fn cell_label(&self, row_ix: usize, col_ix: usize, cx: &App) -> Option<SharedString> {
        let row = self.model_row(row_ix);
        GridDelegate::cell_text(self, row, col_ix, cx).map(Into::into)
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: ferrodb_ui_kit::kit::menu::PopupMenu,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> ferrodb_ui_kit::kit::menu::PopupMenu {
        let row = self.model_row(row_ix);
        let view = self.view.clone();
        let editable = self.model.read(cx).editable();
        let item =
            |label: &str, f: fn(&mut GridView, usize, &mut Window, &mut Context<GridView>)| {
                let view = view.clone();
                PopupMenuItem::new(t(label)).on_click(move |_, window, cx| {
                    let _ = view.update(cx, |v, cx| f(v, row, window, cx));
                })
            };
        menu.item(item("grid-copy-cell", |v, _, _, cx| v.copy_cell(cx)))
            .item(item("grid-copy-row", |v, r, _, cx| {
                v.copy_rows(&[r], CopyFormat::Tsv, cx)
            }))
            .item(item("grid-copy-json", |v, r, _, cx| {
                v.copy_rows(&[r], CopyFormat::Json, cx)
            }))
            .item(item("grid-copy-insert", |v, r, _, cx| {
                v.copy_rows(&[r], CopyFormat::Insert, cx)
            }))
            .separator()
            .item(item("grid-edit-cell", |v, _, w, cx| v.edit_selected(w, cx)).disabled(!editable))
            .item(
                item("grid-set-null", |v, _, w, cx| v.set_selected_null(w, cx)).disabled(!editable),
            )
            .item(item("grid-delete-row", |v, r, _, cx| v.toggle_delete(r, cx)).disabled(!editable))
            .separator()
            .item(item("grid-record-view", |v, _, _, cx| {
                v.record_view = !v.record_view;
                cx.notify();
            }))
    }

    fn has_more(&self, cx: &App) -> bool {
        let m = self.model.read(cx);
        m.has_more && !m.fetching && self.order.is_none()
    }

    fn load_more_threshold(&self) -> usize {
        50
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        ResultModel::fetch_more(&self.model, false, cx);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyFormat {
    Tsv,
    Json,
    Insert,
}

pub struct GridView {
    model: Entity<ResultModel>,
    table: Entity<TableState<GridDelegate>>,
    filter: Entity<InputState>,
    order_by: Entity<InputState>,
    search: Entity<InputState>,
    sort: Option<(usize, ColumnSort)>,
    /// Selected (model row, col).
    selected: Option<(usize, usize)>,
    pub record_view: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<ResultViewEvent> for GridView {}

impl GridView {
    pub fn new(model: Entity<ResultModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let weak = cx.entity().downgrade();
        let delegate = GridDelegate::new(model.clone(), weak, cx);
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .cell_selectable(true)
                .row_selectable(true)
        });
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("WHERE …"));
        let order_by = cx.new(|cx| InputState::new(window, cx).placeholder("ORDER BY …"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t("grid-search")));
        let subs = vec![
            cx.observe(&model, |this, _, cx| {
                this.table.update(cx, |t, cx| {
                    if t.delegate().columns.is_empty() {
                        t.delegate_mut().rebuild_columns(cx);
                    }
                    t.refresh(cx);
                });
                cx.notify();
            }),
            cx.subscribe_in(&table, window, Self::on_table_event),
            cx.subscribe_in(&filter, window, Self::on_filter_event),
            cx.subscribe_in(&order_by, window, Self::on_filter_event),
            cx.subscribe(&search, |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    let q = input.read(cx).value().to_string();
                    let sort = this.sort;
                    this.table.update(cx, |t, cx| {
                        t.delegate_mut().apply_local(&q, sort, cx);
                        t.refresh(cx);
                    });
                    cx.notify();
                }
            }),
        ];
        Self {
            model,
            table,
            filter,
            order_by,
            search,
            sort: None,
            selected: None,
            record_view: false,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn model(&self) -> &Entity<ResultModel> {
        &self.model
    }

    pub fn table(&self) -> &Entity<TableState<GridDelegate>> {
        &self.table
    }

    fn delegate<'a>(&self, cx: &'a App) -> &'a GridDelegate {
        self.table.read(cx).delegate()
    }

    fn column_name(&self, col: usize, cx: &App) -> String {
        table(&self.model, cx)
            .and_then(|t| t.schema.fields().get(col).map(|f| f.name().clone()))
            .unwrap_or_default()
    }

    fn on_table_event(
        &mut self,
        _: &Entity<TableState<GridDelegate>>,
        ev: &TableEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            TableEvent::SelectCell(r, c) => {
                let row = self.delegate(cx).model_row(*r);
                self.select(row, *c, cx);
            }
            TableEvent::SelectRow(r) => {
                let row = self.delegate(cx).model_row(*r);
                self.select(row, self.selected.map_or(0, |s| s.1), cx);
            }
            TableEvent::DoubleClickedCell(r, c) => {
                let row = self.delegate(cx).model_row(*r);
                self.select(row, *c, cx);
                self.start_edit(row, *c, window, cx);
            }
            _ => {}
        }
    }

    /// Selects a cell and publishes it to the value panel.
    pub fn select(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        self.selected = Some((row, col));
        let d = self.delegate(cx);
        let value = d.cell_text(row, col, cx);
        let fields = table(&self.model, cx)
            .map(|t| {
                t.schema
                    .fields()
                    .iter()
                    .enumerate()
                    .map(|(c, f)| {
                        (
                            f.name().clone(),
                            d.cell_text(row, c, cx).unwrap_or_else(|| NULL.into()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ty = table(&self.model, cx)
            .and_then(|t| {
                t.schema
                    .fields()
                    .get(col)
                    .map(|f| format::type_label(f.data_type()))
            })
            .unwrap_or_default();
        inspector::show(
            Inspection {
                title: format!(
                    "{} · {}",
                    self.model.read(cx).title,
                    self.column_name(col, cx)
                ),
                subtitle: t_args(
                    "grid-row-type",
                    &[("row", (row + 1).into()), ("type", ty.into())],
                ),
                value: Some(value.unwrap_or_else(|| NULL.into())),
                fields,
            },
            cx,
        );
        cx.notify();
    }

    pub fn selected(&self) -> Option<(usize, usize)> {
        self.selected
    }

    fn check_editable(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = self.model.read(cx);
        if m.read_only {
            window.push_notification(Notification::warning(t("grid-read-only")), cx);
            return false;
        }
        if !m.capabilities.editable_results {
            window.push_notification(Notification::warning(t("grid-not-editable")), cx);
            return false;
        }
        true
    }

    /// Starts in-place editing: booleans toggle, other kinds open an input.
    pub fn start_edit(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.check_editable(window, cx) {
            return;
        }
        let (kind, current) = {
            let d = self.delegate(cx);
            (d.kinds.get(col).copied(), d.cell_text(row, col, cx))
        };
        if kind == Some(ValueKind::Bool) {
            let next = match current.as_deref() {
                Some("true") => "false",
                _ => "true",
            };
            self.commit_value(row, col, Some(next.into()), window, cx);
            return;
        }
        let input =
            cx.new(|cx| InputState::new(window, cx).default_value(current.unwrap_or_default()));
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, input, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    let text = input.read(cx).value().to_string();
                    this.commit_value(row, col, Some(text), window, cx);
                }
            },
        );
        self._subs.push(sub);
        input.update(cx, |i, cx| i.focus(window, cx));
        self.table.update(cx, |t, cx| {
            t.delegate_mut().editing = Some(((row, col), input));
            cx.notify();
        });
    }

    /// Validates and stores a pending value (`None` = NULL).
    pub fn commit_value(
        &mut self,
        row: usize,
        col: usize,
        value: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (kind, original) = {
            let d = self.delegate(cx);
            (
                d.kinds.get(col).copied().unwrap_or(ValueKind::Other),
                table(&self.model, cx).and_then(|t| t.cell(row, col)),
            )
        };
        if let Err(e) = format::encode_cell(kind, value.as_deref()) {
            window.push_notification(Notification::error(e), cx);
            return;
        }
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.editing = None;
            if value == original {
                d.pending.remove(&(row, col));
            } else {
                d.pending.insert((row, col), value);
            }
            cx.notify();
        });
        self.select(row, col, cx);
        window.focus(&self.focus, cx);
    }

    fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.selected {
            self.start_edit(r, c, window, cx);
        }
    }

    fn set_selected_null(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.selected
            && self.check_editable(window, cx)
        {
            self.commit_value(r, c, None, window, cx);
        }
    }

    fn toggle_delete(&mut self, row: usize, cx: &mut Context<Self>) {
        if !self.model.read(cx).editable() {
            return;
        }
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            if !d.deleted.remove(&row) {
                d.deleted.insert(row);
            }
            cx.notify();
        });
        cx.notify();
    }

    fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.check_editable(window, cx) {
            return;
        }
        let cols = self.delegate(cx).columns.len();
        self.table.update(cx, |t, cx| {
            t.delegate_mut().inserted.push(vec![None; cols]);
            cx.notify();
        });
        window.push_notification(Notification::info(t("grid-row-added")), cx);
        cx.notify();
    }

    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.delegate(cx).change_set();
        let table = self.table.downgrade();
        let view = cx.entity().downgrade();
        changes::review_and_apply(
            &self.model,
            changes,
            move |_, cx| {
                let _ = table.update(cx, |t, cx| {
                    t.delegate_mut().clear_pending();
                    cx.notify();
                });
                let _ = view.update(cx, |_, cx| cx.emit(ResultViewEvent::Applied));
            },
            window,
            cx,
        );
    }

    pub fn revert(&mut self, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().clear_pending();
            cx.notify();
        });
        cx.notify();
    }

    fn copy_cell(&mut self, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.selected {
            let v = self.delegate(cx).cell_text(r, c, cx).unwrap_or_default();
            cx.write_to_clipboard(ClipboardItem::new_string(v));
        }
    }

    /// Copies model rows as TSV (with header), JSON objects or INSERTs.
    pub fn copy_rows(&mut self, rows: &[usize], fmt: CopyFormat, cx: &mut Context<Self>) {
        let text = self.rows_text(rows, fmt, cx);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    pub fn rows_text(&self, rows: &[usize], fmt: CopyFormat, cx: &App) -> String {
        let d = self.delegate(cx);
        let Some(t) = table(&self.model, cx) else {
            return String::new();
        };
        let names: Vec<String> = t.schema.fields().iter().map(|f| f.name().clone()).collect();
        let cell = |r: usize, c: usize| d.cell_text(r, c, cx);
        match fmt {
            CopyFormat::Tsv => {
                let mut out = names.join("\t");
                for &r in rows {
                    out.push('\n');
                    let vals: Vec<String> = (0..names.len())
                        .map(|c| cell(r, c).unwrap_or_default())
                        .collect();
                    out.push_str(&vals.join("\t"));
                }
                out
            }
            CopyFormat::Json => {
                let objs: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|&r| {
                        let map = names
                            .iter()
                            .enumerate()
                            .map(|(c, n)| {
                                let v = match (cell(r, c), d.kinds[c]) {
                                    (None, _) => serde_json::Value::Null,
                                    (Some(v), ValueKind::Integer | ValueKind::Float) => {
                                        serde_json::from_str(&v)
                                            .unwrap_or(serde_json::Value::String(v))
                                    }
                                    (Some(v), ValueKind::Bool) => {
                                        serde_json::Value::Bool(v == "true")
                                    }
                                    (Some(v), _) => serde_json::Value::String(v),
                                };
                                (n.clone(), v)
                            })
                            .collect();
                        serde_json::Value::Object(map)
                    })
                    .collect();
                let v = if objs.len() == 1 {
                    objs.into_iter().next().unwrap_or_default()
                } else {
                    serde_json::Value::Array(objs)
                };
                serde_json::to_string_pretty(&v).unwrap_or_default()
            }
            CopyFormat::Insert => {
                let title = self.model.read(cx).title.clone();
                rows.iter()
                    .map(|&r| {
                        let vals: Vec<String> = (0..names.len())
                            .map(|c| match (cell(r, c), d.kinds[c]) {
                                (None, _) => "NULL".into(),
                                (
                                    Some(v),
                                    ValueKind::Integer | ValueKind::Float | ValueKind::Bool,
                                ) => v,
                                (Some(v), _) => format!("'{}'", v.replace('\'', "''")),
                            })
                            .collect();
                        format!(
                            "INSERT INTO {} ({}) VALUES ({});",
                            table_name(&title),
                            names.join(", "),
                            vals.join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    fn on_filter_event(
        &mut self,
        _: &Entity<InputState>,
        ev: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::PressEnter { .. } = ev {
            let filter = self.filter.read(cx).value().trim().to_string();
            let order = self.order_by.read(cx).value().trim().to_string();
            let stmt = self.model.read(cx).statement.clone();
            cx.emit(ResultViewEvent::Rerun(wrap_query(&stmt, &filter, &order)));
        }
    }

    /// Count / sum / avg / min / max of the selected column over loaded rows.
    pub fn aggregates(&self, cx: &App) -> Option<Aggregates> {
        let (_, col) = self.selected?;
        let t = table(&self.model, cx)?;
        let numeric = matches!(
            format::value_kind(t.schema.fields().get(col)?.data_type()),
            ValueKind::Integer | ValueKind::Float
        );
        let mut a = Aggregates {
            count: 0,
            nulls: 0,
            sum: None,
            min: None,
            max: None,
        };
        for r in 0..t.rows {
            match (numeric, t.number(r, col), t.locate(r, col)) {
                (_, _, Some((arr, i))) if arr.is_null(i) => a.nulls += 1,
                (true, Some(v), _) => {
                    a.count += 1;
                    a.sum = Some(a.sum.unwrap_or(0.0) + v);
                    a.min = Some(a.min.map_or(v, |m: f64| m.min(v)));
                    a.max = Some(a.max.map_or(v, |m: f64| m.max(v)));
                }
                _ => a.count += 1,
            }
        }
        Some(a)
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let sql = matches!(m.capabilities.language, QueryLanguage::Sql(_));
        let editable = m.editable();
        let pending = self.delegate(cx).pending_count();
        h_flex()
            .gap_1()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(sql, |d| {
                d.child(div().w(px(260.)).child(Input::new(&self.filter).xsmall()))
                    .child(div().w(px(180.)).child(Input::new(&self.order_by).xsmall()))
            })
            .child(
                div().w(px(160.)).child(
                    Input::new(&self.search)
                        .xsmall()
                        .prefix(icons::Icon::from(icons::Lucide::Search).xsmall()),
                ),
            )
            .child(div().flex_1())
            .child(
                Button::new("grid-add-row")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Plus)
                    .tooltip(t("grid-add-row"))
                    .disabled(!editable)
                    .on_click(cx.listener(|this, _, w, cx| this.add_row(w, cx))),
            )
            .child(
                Button::new("grid-submit")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Check)
                    .tooltip(t("action-submit-changes"))
                    .disabled(pending == 0)
                    .on_click(cx.listener(|this, _, w, cx| this.submit(w, cx))),
            )
            .child(
                Button::new("grid-revert")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Undo2)
                    .tooltip(t("action-revert-changes"))
                    .disabled(pending == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.revert(cx))),
            )
            .child(
                Button::new("grid-record")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::PanelRight)
                    .tooltip(t("grid-record-view"))
                    .selected(self.record_view)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.record_view = !this.record_view;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("grid-export")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Download)
                    .tooltip(t("action-export-data"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(actions::ExportData), cx)
                    }),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let loaded = m.data.len();
        let shown = self.delegate(cx).order.as_ref().map(Vec::len);
        let pending = self.delegate(cx).pending_count();
        let muted = cx.theme().muted_foreground;
        let page = m.page_size;
        let (has_more, fetching) = (m.has_more, m.fetching);
        h_flex()
            .id("grid-footer")
            .test_support()
            .gap_3()
            .px_2()
            .py_0p5()
            .text_xs()
            .text_color(muted)
            .border_t_1()
            .border_color(cx.theme().border)
            .child(t_args(
                "grid-rows",
                &[
                    ("count", loaded.into()),
                    ("more", if has_more { "yes" } else { "no" }.into()),
                ],
            ))
            .when_some(shown, |d, n| {
                d.child(t_args("grid-shown", &[("count", n.into())]))
            })
            .when_some(self.aggregates(cx), |d, a| d.child(a.to_string()))
            .when(pending > 0, |d| {
                d.child(
                    div()
                        .text_color(cx.theme().warning)
                        .child(t_args("grid-pending", &[("count", pending.into())])),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("grid-page-size")
                    .ghost()
                    .xsmall()
                    .label(t_args("grid-page-size", &[("count", page.into())]))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.update(cx, |m, cx| {
                            let ix = super::PAGE_SIZES.iter().position(|p| *p == m.page_size);
                            m.page_size = super::PAGE_SIZES[ix.map_or(1, |i| (i + 1) % 4)];
                            cx.notify();
                        })
                    })),
            )
            .child(
                Button::new("grid-load-more")
                    .ghost()
                    .xsmall()
                    .label(t("grid-load-more"))
                    .loading(fetching)
                    .disabled(!has_more)
                    .on_click(cx.listener(|this, _, _, cx| {
                        ResultModel::fetch_more(&this.model, false, cx)
                    })),
            )
            .child(
                Button::new("grid-load-all")
                    .ghost()
                    .xsmall()
                    .label(t("grid-load-all"))
                    .disabled(!has_more)
                    .on_click(
                        cx.listener(|this, _, _, cx| {
                            ResultModel::fetch_more(&this.model, true, cx)
                        }),
                    ),
            )
    }

    fn render_record(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let d = self.delegate(cx);
        let row = self.selected.map(|s| s.0).unwrap_or(0);
        let fields: Vec<(String, String, Option<String>)> = table(&self.model, cx)
            .map(|t| {
                t.schema
                    .fields()
                    .iter()
                    .enumerate()
                    .map(|(c, f)| {
                        (
                            f.name().clone(),
                            format::type_label(f.data_type()),
                            d.cell_text(row, c, cx),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let theme = cx.theme();
        v_flex()
            .id("grid-record")
            .test_support()
            .w(px(300.))
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
                    .child(t_args("grid-record-title", &[("row", (row + 1).into())])),
            )
            .children(fields.into_iter().map(|(name, ty, v)| {
                v_flex()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(name)
                            .child(div().text_xs().text_color(theme.muted_foreground).child(ty)),
                    )
                    .child(div().px_1().rounded_sm().bg(theme.muted).map(|d| match v {
                        Some(v) => d.child(v),
                        None => d.italic().text_color(theme.muted_foreground).child(NULL),
                    }))
            }))
    }
}

/// `count 1000 · sum … · avg … · min … · max …`
#[derive(Clone, Debug, PartialEq)]
pub struct Aggregates {
    pub count: usize,
    pub nulls: usize,
    pub sum: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl std::fmt::Display for Aggregates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "count {}", self.count)?;
        if self.nulls > 0 {
            write!(f, " · nulls {}", self.nulls)?;
        }
        if let (Some(s), Some(lo), Some(hi)) = (self.sum, self.min, self.max) {
            let avg = if self.count > 0 {
                s / self.count as f64
            } else {
                0.0
            };
            write!(f, " · sum {s:.2} · avg {avg:.2} · min {lo} · max {hi}")?;
        }
        Ok(())
    }
}

/// Table name guessed from a result title (`orders`, `public.orders`, `Result 1`).
fn table_name(title: &str) -> String {
    let w = title.split_whitespace().last().unwrap_or("t");
    w.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .to_owned()
}

/// `SELECT * FROM (<stmt>) t WHERE <filter> ORDER BY <order>`; the original
/// statement when both are empty.
pub fn wrap_query(stmt: &str, filter: &str, order: &str) -> String {
    let base = stmt.trim().trim_end_matches(';').trim();
    if filter.is_empty() && order.is_empty() {
        return base.to_owned();
    }
    let mut q = format!("SELECT * FROM ({base}) t");
    if !filter.is_empty() {
        q.push_str(&format!(" WHERE {filter}"));
    }
    if !order.is_empty() {
        q.push_str(&format!(" ORDER BY {order}"));
    }
    q
}

impl Focusable for GridView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for GridView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("result-grid")
            .test_support()
            .key_context(GRID_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(|this, _: &actions::CopySelection, _, cx| this.copy_cell(cx)))
            .on_action(cx.listener(|this, _: &actions::FindInResults, w, cx| {
                this.search.update(cx, |s, cx| s.focus(w, cx))
            }))
            .on_action(cx.listener(|this, _: &actions::SubmitChanges, w, cx| this.submit(w, cx)))
            .on_action(cx.listener(|this, _: &actions::RevertChanges, _, cx| this.revert(cx)))
            .on_action(cx.listener(|this, _: &actions::LoadMore, _, cx| {
                ResultModel::fetch_more(&this.model, false, cx)
            }))
            .child(self.render_toolbar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div().flex_1().h_full().min_w_0().child(
                            DataTable::new(&self.table)
                                .stripe(true)
                                .bordered(false)
                                .xsmall(),
                        ),
                    )
                    .when(self.record_view, |d| d.child(self.render_record(cx))),
            )
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn wrapping_filters() {
        assert_eq!(wrap_query("select * from a;", "", ""), "select * from a");
        assert_eq!(
            wrap_query("select * from a", "x > 1", "y desc"),
            "SELECT * FROM (select * from a) t WHERE x > 1 ORDER BY y desc"
        );
    }

    #[::core::prelude::v1::test]
    fn aggregate_text() {
        let a = Aggregates {
            count: 2,
            nulls: 1,
            sum: Some(3.0),
            min: Some(1.0),
            max: Some(2.0),
        };
        assert_eq!(
            a.to_string(),
            "count 2 · nulls 1 · sum 3.00 · avg 1.50 · min 1 · max 2"
        );
    }
}
