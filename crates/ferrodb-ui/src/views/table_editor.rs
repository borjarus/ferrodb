//! Visual table editor with DDL preview (plan 2.8, mockup level: the DDL
//! is generated locally for the preview; real generators per dialect come
//! with `ferrodb-lang` / `ferrodb-meta`).

use ferrodb_api::{NodeRef, ObjectKind};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        dialog::DialogFooter,
        h_flex,
        input::{Input, InputEvent, InputState},
        notification::Notification,
        v_flex,
    },
};

use crate::{
    AppState,
    i18n::{t, t_args},
    output,
};

pub struct ColumnRow {
    pub name: Entity<InputState>,
    pub ty: Entity<InputState>,
    pub default: Entity<InputState>,
    pub nullable: bool,
    pub primary: bool,
}

pub struct TableEditor {
    /// Table being modified (`None` = new table).
    pub source: Option<NodeRef>,
    pub name: Entity<InputState>,
    pub columns: Vec<ColumnRow>,
    /// Columns as loaded (to produce ALTER statements).
    original: Vec<(String, String)>,
    _subs: Vec<Subscription>,
}

impl TableEditor {
    pub fn new(source: Option<NodeRef>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let table = source
            .as_ref()
            .and_then(|n| n.name())
            .unwrap_or("new_table")
            .to_owned();
        let name = cx.new(|cx| InputState::new(window, cx).default_value(table));
        let sub = cx.subscribe(&name, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify()
            }
        });
        let mut this = Self {
            source: source.clone(),
            name,
            columns: Vec::new(),
            original: Vec::new(),
            _subs: vec![sub],
        };
        match source {
            Some(node) => this.load(node, window, cx),
            None => {
                this.add_column("id", "bigint", false, true, window, cx);
                this.add_column("name", "text", true, false, window, cx);
            }
        }
        this
    }

    fn load(&mut self, node: NodeRef, window: &mut Window, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.metadata().children(node).await });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(objs) = task.await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                for o in objs.iter().filter(|o| o.kind == ObjectKind::Column) {
                    let ty = sql_type(o.detail.as_deref().unwrap_or("text"));
                    this.original.push((o.name.clone(), ty.clone()));
                    let pk = o.name == "id";
                    this.add_column(&o.name, &ty, !pk, pk, window, cx);
                }
            });
        })
        .detach();
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
        let input = |v: &str, window: &mut Window, cx: &mut Context<Self>| {
            let v = v.to_owned();
            let e = cx.new(|cx| InputState::new(window, cx).default_value(v));
            let sub = cx.subscribe(&e, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify()
                }
            });
            (e, sub)
        };
        let (name, s1) = input(name, window, cx);
        let (ty, s2) = input(ty, window, cx);
        let (default, s3) = input("", window, cx);
        self._subs.extend([s1, s2, s3]);
        self.columns.push(ColumnRow {
            name,
            ty,
            default,
            nullable,
            primary,
        });
        cx.notify();
    }

    /// `CREATE TABLE` for a new table, `ALTER TABLE` statements for an existing one.
    pub fn ddl(&self, cx: &App) -> String {
        let table = self.name.read(cx).value().to_string();
        let cols: Vec<(String, String, String, bool, bool)> = self
            .columns
            .iter()
            .map(|c| {
                (
                    c.name.read(cx).value().trim().to_string(),
                    c.ty.read(cx).value().trim().to_string(),
                    c.default.read(cx).value().trim().to_string(),
                    c.nullable,
                    c.primary,
                )
            })
            .filter(|c| !c.0.is_empty())
            .collect();
        let def = |(n, ty, d, null, _): &(String, String, String, bool, bool)| {
            let mut s = format!("{n} {ty}");
            if !null {
                s.push_str(" NOT NULL");
            }
            if !d.is_empty() {
                s.push_str(&format!(" DEFAULT {d}"));
            }
            s
        };
        if self.source.is_none() {
            let mut lines: Vec<String> = cols.iter().map(|c| format!("    {}", def(c))).collect();
            let pk: Vec<&str> = cols.iter().filter(|c| c.4).map(|c| c.0.as_str()).collect();
            if !pk.is_empty() {
                lines.push(format!("    PRIMARY KEY ({})", pk.join(", ")));
            }
            return format!("CREATE TABLE {table} (\n{}\n);", lines.join(",\n"));
        }
        let mut out = Vec::new();
        let original_name = self
            .source
            .as_ref()
            .and_then(|n| n.name())
            .unwrap_or_default();
        if table != original_name {
            out.push(format!("ALTER TABLE {original_name} RENAME TO {table};"));
        }
        for c in &cols {
            match self.original.iter().find(|(n, _)| *n == c.0) {
                None => out.push(format!("ALTER TABLE {table} ADD COLUMN {};", def(c))),
                Some((_, ty)) if *ty != c.1 => out.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {} TYPE {};",
                    c.0, c.1
                )),
                _ => {}
            }
        }
        for (n, _) in &self.original {
            if !cols.iter().any(|c| &c.0 == n) {
                out.push(format!("ALTER TABLE {table} DROP COLUMN {n};"));
            }
        }
        if out.is_empty() {
            t("table-editor-no-changes")
        } else {
            out.join("\n")
        }
    }
}

/// Arrow type name (as the mock reports columns) → SQL type for the editor.
fn sql_type(arrow: &str) -> String {
    match arrow {
        "Int64" => "bigint",
        "Int32" => "integer",
        "Float64" => "double precision",
        "Boolean" => "boolean",
        "Utf8" => "text",
        s if s.starts_with("Timestamp") => "timestamp",
        s => return s.to_lowercase(),
    }
    .into()
}

impl Render for TableEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = h_flex()
            .gap_2()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.muted_foreground)
            .child(div().w(px(160.)).child(t("table-editor-column")))
            .child(div().w(px(140.)).child(t("table-editor-type")))
            .child(div().w(px(120.)).child(t("table-editor-default")))
            .child(div().w(px(60.)).child("NULL"))
            .child(div().w(px(40.)).child("PK"));
        let rows = self.columns.iter().enumerate().map(|(i, c)| {
            h_flex()
                .gap_2()
                .child(div().w(px(160.)).child(Input::new(&c.name).xsmall()))
                .child(div().w(px(140.)).child(Input::new(&c.ty).xsmall()))
                .child(div().w(px(120.)).child(Input::new(&c.default).xsmall()))
                .child(
                    div().w(px(60.)).child(
                        Checkbox::new(("te-null", i))
                            .checked(c.nullable)
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                this.columns[i].nullable = *v;
                                cx.notify();
                            })),
                    ),
                )
                .child(
                    div().w(px(40.)).child(
                        Checkbox::new(("te-pk", i))
                            .checked(c.primary)
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                this.columns[i].primary = *v;
                                cx.notify();
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
                            cx.notify();
                        })),
                )
        });
        let ddl = self.ddl(cx);
        v_flex()
            .id("table-editor")
            .test_support()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().w(px(100.)).child(t("table-editor-name")))
                    .child(div().w(px(240.)).child(Input::new(&self.name).small())),
            )
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
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("table-editor-ddl")),
            )
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

pub fn open(source: Option<NodeRef>, window: &mut Window, cx: &mut App) {
    let editor = cx.new(|cx| TableEditor::new(source.clone(), window, cx));
    cx.set_global(LastTableEditor(Some(editor.clone())));
    let title = match source.as_ref().and_then(|n| n.name()) {
        Some(n) => t_args("table-editor-modify", &[("name", n.to_owned().into())]),
        None => t("action-new-table"),
    };
    window.open_dialog(cx, move |dialog, _, _| {
        let e = editor.clone();
        dialog
            .title(title.clone())
            .w(px(720.))
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
                            .on_click(move |_, w, cx| {
                                let ddl = e.read(cx).ddl(cx);
                                w.close_dialog(cx);
                                output::info(ddl, cx);
                                w.push_notification(
                                    Notification::info(t("table-editor-mock-applied")),
                                    cx,
                                );
                            }),
                    ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn arrow_types_map_to_sql() {
        assert_eq!(sql_type("Int64"), "bigint");
        assert_eq!(sql_type("Timestamp(Millisecond, None)"), "timestamp");
    }
}
