//! Visual table editor (plan, phase 7): columns, primary key, indexes and
//! foreign keys of a new or existing table. Every change asks the backend
//! for the script ([`MetadataService::design_script`]: `CREATE TABLE` or
//! `ALTER TABLE` in the source's dialect; a SQLite table is rebuilt when it
//! must be) and shows it with its warnings; Apply runs it
//! ([`MetadataService::apply_script`]) and refreshes the explorer.
//!
//! [`MetadataService::design_script`]: ferrodb_api::MetadataService::design_script
//! [`MetadataService::apply_script`]: ferrodb_api::MetadataService::apply_script

use std::time::Duration;

use ferrodb_api::{
    ColumnDesign, DesignScript, ForeignKeyDesign, IndexDesign, NodeRef, ObjectKind, TableDesign,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        dialog::DialogFooter,
        h_flex,
        input::{Input, InputEvent, InputState},
        notification::Notification,
        tab::{Tab, TabBar},
        v_flex,
    },
};

use crate::{
    AppState,
    i18n::{t, t_args},
    output,
    views::schema_compare::container_of,
    workspace,
};

/// Delay between the last edit and asking for the script.
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(250);

pub struct ColumnRow {
    /// Name as loaded (`None` = added in the editor).
    pub original: Option<String>,
    pub name: Entity<InputState>,
    pub ty: Entity<InputState>,
    pub default: Entity<InputState>,
    pub nullable: bool,
    pub primary: bool,
    pub auto_increment: bool,
    pub comment: Option<String>,
}

pub struct IndexRow {
    pub name: Entity<InputState>,
    /// Comma-separated columns.
    pub columns: Entity<InputState>,
    pub unique: bool,
}

pub struct ForeignKeyRow {
    pub name: Option<String>,
    pub columns: Entity<InputState>,
    pub ref_table: Entity<InputState>,
    pub ref_columns: Entity<InputState>,
    pub on_delete: Entity<InputState>,
    pub on_update: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorTab {
    Columns,
    Indexes,
    ForeignKeys,
}

pub struct TableEditor {
    /// Schema / database the table is in.
    pub container: Option<NodeRef>,
    /// The table being modified (`None` = a new table).
    pub source: Option<NodeRef>,
    pub name: Entity<InputState>,
    pub columns: Vec<ColumnRow>,
    pub indexes: Vec<IndexRow>,
    pub foreign_keys: Vec<ForeignKeyRow>,
    /// What the editor does not show, kept from the loaded design.
    loaded: Option<TableDesign>,
    pub tab: EditorTab,
    pub script: Option<DesignScript>,
    pub error: Option<String>,
    pub applying: bool,
    preview: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

fn split_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_owned())
        .filter(|x| !x.is_empty())
        .collect()
}

impl TableEditor {
    pub fn new(node: Option<NodeRef>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = node
            .as_ref()
            .filter(|n| n.kind() == Some(ObjectKind::Table))
            .cloned();
        let container = node
            .as_ref()
            .map(container_of)
            .filter(|c| !c.path.is_empty());
        let name_text = table
            .as_ref()
            .and_then(|n| n.name())
            .unwrap_or("new_table")
            .to_owned();
        let mut this = Self {
            container,
            source: table.clone(),
            name: cx.new(|cx| InputState::new(window, cx)),
            columns: Vec::new(),
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
            loaded: None,
            tab: EditorTab::Columns,
            script: None,
            error: None,
            applying: false,
            preview: None,
            _subs: Vec::new(),
        };
        this.name = this.input(&name_text, window, cx);
        match table {
            Some(node) => this.load(node, window, cx),
            None => {
                this.add_column("id", "bigint", false, true, window, cx);
                this.columns[0].auto_increment = true;
                this.add_column("name", "text", true, false, window, cx);
                this.schedule_preview(cx);
            }
        }
        this
    }

    fn input(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let v = value.to_owned();
        let e = cx.new(|cx| InputState::new(window, cx).default_value(v));
        self._subs
            .push(cx.subscribe(&e, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.schedule_preview(cx);
                }
            }));
        e
    }

    fn load(&mut self, node: NodeRef, window: &mut Window, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.metadata().table_design(node).await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, window, cx| match r {
                Ok(d) => this.fill(d, window, cx),
                Err(e) => {
                    this.error = Some(e.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Shows a loaded design.
    pub fn fill(&mut self, d: TableDesign, window: &mut Window, cx: &mut Context<Self>) {
        self.columns.clear();
        self.indexes.clear();
        self.foreign_keys.clear();
        for c in &d.columns {
            let pk = d.primary_key.contains(&c.name);
            self.add_column(&c.name, &c.data_type, c.nullable, pk, window, cx);
            let row = self.columns.last_mut().expect("just added");
            row.original = c.original.clone();
            row.auto_increment = c.auto_increment;
            row.comment = c.comment.clone();
            if let Some(def) = &c.default {
                row.default
                    .update(cx, |i, cx| i.set_value(def.clone(), window, cx));
            }
        }
        for i in &d.indexes {
            self.add_index(&i.name, &i.columns.join(", "), i.unique, window, cx);
        }
        for f in &d.foreign_keys {
            self.add_foreign_key(f, window, cx);
        }
        self.loaded = Some(d);
        self.schedule_preview(cx);
    }

    pub fn add_column(
        &mut self,
        name: &str,
        ty: &str,
        nullable: bool,
        primary: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.input(name, window, cx);
        let ty = self.input(ty, window, cx);
        let default = self.input("", window, cx);
        self.columns.push(ColumnRow {
            original: None,
            name,
            ty,
            default,
            nullable,
            primary,
            auto_increment: false,
            comment: None,
        });
        self.schedule_preview(cx);
    }

    pub fn add_index(
        &mut self,
        name: &str,
        columns: &str,
        unique: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.input(name, window, cx);
        let columns = self.input(columns, window, cx);
        self.indexes.push(IndexRow {
            name,
            columns,
            unique,
        });
        self.schedule_preview(cx);
    }

    pub fn add_foreign_key(
        &mut self,
        f: &ForeignKeyDesign,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let columns = self.input(&f.columns.join(", "), window, cx);
        let ref_table = self.input(&f.ref_table, window, cx);
        let ref_columns = self.input(&f.ref_columns.join(", "), window, cx);
        let on_delete = self.input(f.on_delete.as_deref().unwrap_or(""), window, cx);
        self.foreign_keys.push(ForeignKeyRow {
            name: f.name.clone(),
            columns,
            ref_table,
            ref_columns,
            on_delete,
            on_update: f.on_update.clone(),
        });
        self.schedule_preview(cx);
    }

    /// The design as edited.
    pub fn design(&self, cx: &App) -> TableDesign {
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_owned();
        let loaded = self.loaded.clone().unwrap_or_default();
        TableDesign {
            original: self
                .source
                .as_ref()
                .and_then(|n| n.name())
                .map(str::to_owned),
            name: v(&self.name),
            comment: loaded.comment.clone(),
            columns: self
                .columns
                .iter()
                .map(|c| ColumnDesign {
                    original: c.original.clone(),
                    name: v(&c.name),
                    data_type: v(&c.ty),
                    nullable: c.nullable,
                    default: Some(v(&c.default)).filter(|d| !d.is_empty()),
                    comment: c.comment.clone(),
                    auto_increment: c.auto_increment,
                })
                .collect(),
            primary_key: self
                .columns
                .iter()
                .filter(|c| c.primary)
                .map(|c| v(&c.name))
                .filter(|n| !n.is_empty())
                .collect(),
            uniques: loaded.uniques.clone(),
            foreign_keys: self
                .foreign_keys
                .iter()
                .map(|f| ForeignKeyDesign {
                    name: f.name.clone(),
                    columns: split_list(&v(&f.columns)),
                    ref_table: v(&f.ref_table),
                    ref_columns: split_list(&v(&f.ref_columns)),
                    on_delete: Some(v(&f.on_delete)).filter(|x| !x.is_empty()),
                    on_update: f.on_update.clone(),
                })
                .collect(),
            indexes: self
                .indexes
                .iter()
                .map(|i| IndexDesign {
                    name: v(&i.name),
                    columns: split_list(&v(&i.columns)),
                    unique: i.unique,
                })
                .collect(),
            checks: loaded.checks.clone(),
        }
    }

    /// Asks the backend for the script after a short pause.
    pub fn schedule_preview(&mut self, cx: &mut Context<Self>) {
        let Some(container) = self.container.clone() else {
            self.error = Some(t("table-editor-no-schema"));
            cx.notify();
            return;
        };
        self.preview = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            let Ok((design, backend)) =
                this.update(cx, |this, cx| (this.design(cx), AppState::backend(cx)))
            else {
                return;
            };
            let r = cx
                .background_executor()
                .spawn(async move { backend.metadata().design_script(container, design).await })
                .await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(s) => {
                        this.script = Some(s);
                        this.error = None;
                    }
                    Err(e) => {
                        this.script = None;
                        this.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The script shown in the preview.
    pub fn ddl(&self) -> String {
        match (&self.script, &self.error) {
            (_, Some(e)) => format!("-- {e}"),
            (Some(s), _) if s.statements.is_empty() => t("table-editor-no-changes"),
            (Some(s), _) => s.text(),
            (None, None) => String::new(),
        }
    }

    /// Runs the script; returns whether it was started.
    pub fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (Some(container), Some(script)) = (self.container.clone(), self.script.clone()) else {
            return false;
        };
        if script.statements.is_empty() || self.applying {
            return false;
        }
        self.applying = true;
        let backend = AppState::backend(cx);
        let node = container.clone();
        let statements = script.statements.clone();
        let task = cx
            .background_executor()
            .spawn(async move { backend.metadata().apply_script(node, statements).await });
        let text = script.text();
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.applying = false;
                match r {
                    Ok(_) => {
                        output::info(text.clone(), cx);
                        window.close_dialog(cx);
                        window.push_notification(
                            Notification::success(t("table-editor-applied")),
                            cx,
                        );
                        workspace::refresh_explorer_node(container.clone(), cx);
                    }
                    Err(e) => {
                        this.error = Some(e.to_string());
                        window.push_notification(Notification::error(e.to_string()), cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    fn columns_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = h_flex()
            .gap_2()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(div().w(px(160.)).child(t("table-editor-column")))
            .child(div().w(px(150.)).child(t("table-editor-type")))
            .child(div().w(px(120.)).child(t("table-editor-default")))
            .child(div().w(px(50.)).child("NULL"))
            .child(div().w(px(40.)).child("PK"))
            .child(div().w(px(50.)).child(t("table-editor-auto")));
        let rows = self.columns.iter().enumerate().map(|(i, c)| {
            h_flex()
                .gap_2()
                .child(div().w(px(160.)).child(Input::new(&c.name).xsmall()))
                .child(div().w(px(150.)).child(Input::new(&c.ty).xsmall()))
                .child(div().w(px(120.)).child(Input::new(&c.default).xsmall()))
                .child(
                    div().w(px(50.)).child(
                        Checkbox::new(("te-null", i))
                            .checked(c.nullable)
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                this.columns[i].nullable = *v;
                                this.schedule_preview(cx);
                            })),
                    ),
                )
                .child(
                    div().w(px(40.)).child(
                        Checkbox::new(("te-pk", i))
                            .checked(c.primary)
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                this.columns[i].primary = *v;
                                if *v {
                                    this.columns[i].nullable = false;
                                }
                                this.schedule_preview(cx);
                            })),
                    ),
                )
                .child(
                    div().w(px(50.)).child(
                        Checkbox::new(("te-auto", i))
                            .checked(c.auto_increment)
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                this.columns[i].auto_increment = *v;
                                this.schedule_preview(cx);
                            })),
                    ),
                )
                .child(
                    Button::new(("te-remove", i))
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Trash)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.columns.remove(i);
                            this.schedule_preview(cx);
                        })),
                )
        });
        v_flex()
            .gap_1()
            .child(header)
            .child(
                v_flex()
                    .id("te-columns")
                    .gap_1()
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(
                Button::new("te-add")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Plus)
                    .label(t("table-editor-add-column"))
                    .on_click(cx.listener(|this, _, w, cx| {
                        this.add_column("", "text", true, false, w, cx)
                    })),
            )
    }

    fn indexes_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.indexes.iter().enumerate().map(|(i, x)| {
            h_flex()
                .gap_2()
                .child(div().w(px(200.)).child(Input::new(&x.name).xsmall()))
                .child(div().w(px(300.)).child(Input::new(&x.columns).xsmall()))
                .child(
                    Checkbox::new(("te-unique", i))
                        .label(t("table-editor-unique"))
                        .checked(x.unique)
                        .on_click(cx.listener(move |this, v: &bool, _, cx| {
                            this.indexes[i].unique = *v;
                            this.schedule_preview(cx);
                        })),
                )
                .child(
                    Button::new(("te-ix-remove", i))
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Trash)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.indexes.remove(i);
                            this.schedule_preview(cx);
                        })),
                )
        });
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(div().w(px(200.)).child(t("table-editor-index-name")))
                    .child(div().w(px(300.)).child(t("table-editor-index-columns"))),
            )
            .children(rows)
            .child(
                Button::new("te-add-index")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Plus)
                    .label(t("table-editor-add-index"))
                    .on_click(cx.listener(|this, _, w, cx| {
                        let table = this.name.read(cx).value().trim().to_owned();
                        let n = this.indexes.len() + 1;
                        this.add_index(&format!("ix_{table}_{n}"), "", false, w, cx)
                    })),
            )
    }

    fn foreign_keys_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let rows = self.foreign_keys.iter().enumerate().map(|(i, f)| {
            h_flex()
                .gap_2()
                .child(div().w(px(150.)).child(Input::new(&f.columns).xsmall()))
                .child(div().w(px(150.)).child(Input::new(&f.ref_table).xsmall()))
                .child(div().w(px(150.)).child(Input::new(&f.ref_columns).xsmall()))
                .child(div().w(px(110.)).child(Input::new(&f.on_delete).xsmall()))
                .child(
                    Button::new(("te-fk-remove", i))
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Trash)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.foreign_keys.remove(i);
                            this.schedule_preview(cx);
                        })),
                )
        });
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    .child(div().w(px(150.)).child(t("table-editor-fk-columns")))
                    .child(div().w(px(150.)).child(t("table-editor-fk-table")))
                    .child(div().w(px(150.)).child(t("table-editor-fk-ref-columns")))
                    .child(div().w(px(110.)).child(t("table-editor-fk-on-delete"))),
            )
            .children(rows)
            .child(
                Button::new("te-add-fk")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Plus)
                    .label(t("table-editor-add-fk"))
                    .on_click(cx.listener(|this, _, w, cx| {
                        this.add_foreign_key(&ForeignKeyDesign::default(), w, cx)
                    })),
            )
    }
}

impl Render for TableEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tab_ix = match self.tab {
            EditorTab::Columns => 0,
            EditorTab::Indexes => 1,
            EditorTab::ForeignKeys => 2,
        };
        let body = match self.tab {
            EditorTab::Columns => self.columns_view(cx).into_any_element(),
            EditorTab::Indexes => self.indexes_view(cx).into_any_element(),
            EditorTab::ForeignKeys => self.foreign_keys_view(cx).into_any_element(),
        };
        let warnings = self
            .script
            .as_ref()
            .map(|s| s.warnings.clone())
            .unwrap_or_default();
        let ddl = self.ddl();
        let place = self
            .container
            .as_ref()
            .map(crate::views::schema_compare::label)
            .unwrap_or_default();
        v_flex()
            .id("table-editor")
            .test_support()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(100.)).child(t("table-editor-name")))
                    .child(div().w(px(240.)).child(Input::new(&self.name).small()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(place),
                    ),
            )
            .child(
                TabBar::new("te-tabs")
                    .xsmall()
                    .selected_index(tab_ix)
                    .child(Tab::new().label(t("table-editor-columns")))
                    .child(Tab::new().label(t_args(
                        "table-editor-indexes",
                        &[("count", self.indexes.len().into())],
                    )))
                    .child(Tab::new().label(t_args(
                        "table-editor-foreign-keys",
                        &[("count", self.foreign_keys.len().into())],
                    )))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.tab = [
                            EditorTab::Columns,
                            EditorTab::Indexes,
                            EditorTab::ForeignKeys,
                        ][*ix];
                        cx.notify();
                    })),
            )
            .child(body)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("table-editor-ddl")),
            )
            .children(warnings.into_iter().map(|w| {
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(icons::Icon::from(icons::Lucide::TriangleAlert).xsmall())
                    .child(w)
            }))
            .child(
                div()
                    .id("te-ddl")
                    .test_support()
                    .p_2()
                    .rounded_md()
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .child(ddl),
            )
    }
}

/// Most recent editor (UI tests).
#[derive(Default)]
pub struct LastTableEditor(pub Option<Entity<TableEditor>>);
impl Global for LastTableEditor {}

/// Opens the editor: a table node edits it, any other node of a schema
/// creates a new table there.
pub fn open(node: Option<NodeRef>, window: &mut Window, cx: &mut App) {
    let editor = cx.new(|cx| TableEditor::new(node.clone(), window, cx));
    cx.set_global(LastTableEditor(Some(editor.clone())));
    let title = match node
        .as_ref()
        .filter(|n| n.kind() == Some(ObjectKind::Table))
        .and_then(|n| n.name())
    {
        Some(n) => t_args("table-editor-modify", &[("name", n.to_owned().into())]),
        None => t("action-new-table"),
    };
    window.open_dialog(cx, move |dialog, _, cx| {
        let e = editor.clone();
        let ready = e
            .read(cx)
            .script
            .as_ref()
            .is_some_and(|s| !s.statements.is_empty())
            && !e.read(cx).applying;
        dialog
            .title(title.clone())
            .w(px(820.))
            .child(editor.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("te-cancel")
                            .label(t("button-cancel"))
                            .on_click(|_, w, cx| w.close_dialog(cx)),
                    )
                    .child(
                        Button::new("te-apply")
                            .primary()
                            .label(t("button-apply"))
                            .disabled(!ready)
                            .on_click(move |_, w, cx| {
                                e.update(cx, |e, cx| {
                                    e.apply(w, cx);
                                });
                            }),
                    ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn lists_are_split() {
        assert_eq!(split_list(" a, b ,,c"), vec!["a", "b", "c"]);
        assert!(split_list("").is_empty());
    }
}
