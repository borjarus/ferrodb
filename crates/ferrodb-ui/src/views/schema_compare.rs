//! Schema comparison (plan, phase 7): pick a source and a target schema
//! (any two tabular data sources), list the differences, show the DDL of
//! both sides of the selected object, and the migration script that makes
//! the target like the source — to open in a console or apply directly.

use ferrodb_api::{
    CompareOptions, DataModel, DataSourceId, DiffKind, NodeRef, ObjectKind, SchemaComparison,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        checkbox::Checkbox,
        h_flex,
        menu::{DropdownMenu as _, PopupMenuItem},
        notification::Notification,
        v_flex,
    },
};

use crate::{
    AppState,
    i18n::{t, t_args},
    output,
    views::diff::DiffView,
    workspace,
};

/// A schema / database a comparison can use.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerChoice {
    pub node: NodeRef,
    /// `shop / public`.
    pub label: String,
}

pub struct SchemaCompareView {
    pub source: Option<NodeRef>,
    pub target: Option<NodeRef>,
    /// Containers of each tabular data source, loaded on demand.
    pub choices: Vec<(DataSourceId, String, Vec<ContainerChoice>)>,
    pub opts: CompareOptions,
    pub result: Option<SchemaComparison>,
    pub selected: Option<usize>,
    pub running: bool,
    pub error: Option<String>,
    diff: Option<Entity<DiffView>>,
}

/// The schema of a node (itself, or the schema / database above it).
pub fn container_of(node: &NodeRef) -> NodeRef {
    let mut n = node.clone();
    while n.kind().is_some_and(|k| {
        !matches!(
            k,
            ObjectKind::Schema | ObjectKind::Dataset | ObjectKind::Keyspace
        )
    }) && n.path.len() > 1
    {
        n.path.pop();
    }
    n
}

/// `shop / public` for a container node.
pub fn label(node: &NodeRef) -> String {
    node.path
        .iter()
        .map(|(_, n)| n.as_str())
        .collect::<Vec<_>>()
        .join(" / ")
}

impl SchemaCompareView {
    pub fn new(source: Option<NodeRef>, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            source: source.as_ref().map(container_of),
            target: None,
            choices: Vec::new(),
            opts: CompareOptions::default(),
            result: None,
            selected: None,
            running: false,
            error: None,
            diff: None,
        };
        this.load_choices(cx);
        this
    }

    /// Lists the schemas of every tabular source (two levels deep at most).
    fn load_choices(&mut self, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let sources: Vec<(DataSourceId, String)> = backend
            .data_sources()
            .list()
            .into_iter()
            .filter(|s| s.capabilities.data_model == DataModel::Tabular)
            .map(|s| (s.config.id.clone(), s.config.name.clone()))
            .collect();
        let task = cx.background_executor().spawn(async move {
            let mut out = Vec::new();
            for (id, name) in sources {
                let m = backend.metadata();
                let root = m
                    .children(NodeRef::root(id.clone()))
                    .await
                    .unwrap_or_default();
                let mut found = Vec::new();
                for o in root.iter().take(20) {
                    if matches!(o.kind, ObjectKind::Database) {
                        let kids = m.children(o.node.clone()).await.unwrap_or_default();
                        let schemas: Vec<ContainerChoice> = kids
                            .into_iter()
                            .filter(|k| matches!(k.kind, ObjectKind::Schema | ObjectKind::Dataset))
                            .map(|k| ContainerChoice {
                                label: label(&k.node),
                                node: k.node,
                            })
                            .collect();
                        if schemas.is_empty() {
                            found.push(ContainerChoice {
                                label: label(&o.node),
                                node: o.node.clone(),
                            });
                        }
                        found.extend(schemas);
                    } else if matches!(o.kind, ObjectKind::Schema | ObjectKind::Dataset) {
                        found.push(ContainerChoice {
                            label: label(&o.node),
                            node: o.node.clone(),
                        });
                    }
                }
                out.push((id, name, found));
            }
            out
        });
        cx.spawn(async move |this, cx| {
            let choices = task.await;
            let _ = this.update(cx, |this, cx| {
                this.choices = choices;
                if this.source.is_none() {
                    this.source = this
                        .choices
                        .iter()
                        .find_map(|(_, _, c)| c.first())
                        .map(|c| c.node.clone());
                }
                if this.target.is_none() {
                    this.target = this.default_target();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The same schema in another source (production first), else another
    /// schema of the same source.
    fn default_target(&self) -> Option<NodeRef> {
        let src = self.source.as_ref()?;
        let name = src.name();
        let others = self
            .choices
            .iter()
            .filter(|(id, _, _)| *id != src.data_source);
        let mut best = None;
        for (id, _, cs) in others {
            if let Some(c) = cs.iter().find(|c| c.node.name() == name) {
                if id.as_str().contains("prod") {
                    return Some(c.node.clone());
                }
                best.get_or_insert(c.node.clone());
            }
        }
        best.or_else(|| {
            self.choices
                .iter()
                .find(|(id, _, _)| *id == src.data_source)
                .and_then(|(_, _, cs)| cs.iter().find(|c| c.node != *src))
                .map(|c| c.node.clone())
        })
    }

    pub fn set_source(&mut self, node: NodeRef, cx: &mut Context<Self>) {
        self.source = Some(node);
        self.result = None;
        cx.notify();
    }

    pub fn set_target(&mut self, node: NodeRef, cx: &mut Context<Self>) {
        self.target = Some(node);
        self.result = None;
        cx.notify();
    }

    pub fn compare(&mut self, cx: &mut Context<Self>) {
        let (Some(s), Some(t)) = (self.source.clone(), self.target.clone()) else {
            self.error = Some(crate::i18n::t("compare-pick-both"));
            cx.notify();
            return;
        };
        self.running = true;
        self.error = None;
        let backend = AppState::backend(cx);
        let opts = self.opts.clone();
        let task = cx
            .background_executor()
            .spawn(async move { backend.metadata().compare_schemas(s, t, opts).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.running = false;
                match r {
                    Ok(r) => {
                        this.result = Some(r);
                        this.selected = None;
                        this.diff = None;
                    }
                    Err(e) => this.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        self.diff = self
            .result
            .as_ref()
            .and_then(|r| r.items.get(ix))
            .map(|it| {
                let (a, b) = (
                    it.source_ddl.clone().unwrap_or_default(),
                    it.target_ddl.clone().unwrap_or_default(),
                );
                cx.new(|cx| {
                    DiffView::new(t("compare-target"), &b, t("compare-source"), &a, "sql", cx)
                })
            });
        cx.notify();
    }

    fn script(&self) -> String {
        self.result
            .as_ref()
            .map(|r| r.script.clone())
            .unwrap_or_default()
    }

    /// Opens the migration script in a console of the target source.
    pub fn open_in_console(&self, window: &mut Window, cx: &mut App) {
        let (Some(target), script) = (self.target.clone(), self.script()) else {
            return;
        };
        workspace::open_console(Some(target.data_source.clone()), script, window, cx);
    }

    /// Runs the migration script on the target, then compares again.
    pub fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(target), Some(r)) = (self.target.clone(), self.result.as_ref()) else {
            return;
        };
        let statements: Vec<String> = r.items.iter().flat_map(|i| i.statements.clone()).collect();
        if statements.is_empty() {
            return;
        }
        // Statements of each item are ordered; the full script orders
        // across items: run the script's statements in its order.
        let ordered = split_script(&r.script);
        let statements = if ordered.len() == statements.len() {
            ordered
        } else {
            statements
        };
        self.running = true;
        let backend = AppState::backend(cx);
        let node = target.clone();
        let task = cx
            .background_executor()
            .spawn(async move { backend.metadata().apply_script(node, statements).await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.running = false;
                match r {
                    Ok(n) => {
                        let msg = t_args("compare-applied", &[("count", n.into())]);
                        output::info(msg.clone(), cx);
                        window.push_notification(Notification::success(msg), cx);
                        workspace::refresh_explorer_node(target.clone(), cx);
                        this.compare(cx);
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
    }

    fn picker(
        &self,
        id: &'static str,
        current: &Option<NodeRef>,
        source: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let label_text = current
            .as_ref()
            .map(|n| {
                let ds = self
                    .choices
                    .iter()
                    .find(|(id, _, _)| *id == n.data_source)
                    .map(|(_, name, _)| name.clone())
                    .unwrap_or_else(|| n.data_source.to_string());
                format!("{ds}: {}", label(n))
            })
            .unwrap_or_else(|| t("compare-choose"));
        let choices = self.choices.clone();
        let weak = cx.entity().downgrade();
        Button::new(id)
            .small()
            .icon(icons::Lucide::Layers)
            .label(label_text)
            .dropdown_menu(move |menu, _, _| {
                choices.iter().fold(menu, |menu, (_, name, cs)| {
                    cs.iter().fold(menu, |menu, c| {
                        let (node, weak) = (c.node.clone(), weak.clone());
                        menu.item(PopupMenuItem::new(format!("{name}: {}", c.label)).on_click(
                            move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    if source {
                                        this.set_source(node.clone(), cx)
                                    } else {
                                        this.set_target(node.clone(), cx)
                                    }
                                });
                            },
                        ))
                    })
                })
            })
            .into_any_element()
    }
}

/// Statements of a generated script (one per `;`-terminated line group).
pub fn split_script(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for line in script.lines() {
        let u = line.trim().to_ascii_uppercase();
        if u.ends_with("BEGIN") || u.contains(" BEGIN ") {
            depth += 1;
        }
        if u.starts_with("END") && depth > 0 {
            depth -= 1;
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
        if depth == 0 && line.trim_end().ends_with(';') {
            out.push(std::mem::take(&mut cur).trim().to_owned());
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out.retain(|s| !s.is_empty());
    out
}

fn kind_label(k: DiffKind) -> &'static str {
    match k {
        DiffKind::Added => "+",
        DiffKind::Removed => "−",
        DiffKind::Changed => "≠",
    }
}

impl Render for SchemaCompareView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let source = self.source.clone();
        let target = self.target.clone();
        let src_picker = self.picker("compare-source", &source, true, cx);
        let tgt_picker = self.picker("compare-target", &target, false, cx);
        let items = self
            .result
            .as_ref()
            .map(|r| r.items.clone())
            .unwrap_or_default();
        let warnings = self
            .result
            .as_ref()
            .map(|r| r.warnings.clone())
            .unwrap_or_default();
        let script = self.script();
        let status = if self.running {
            t("compare-running")
        } else if let Some(e) = &self.error {
            e.clone()
        } else if let Some(r) = &self.result {
            if r.items.is_empty() {
                t("compare-identical")
            } else {
                t_args("compare-summary", &[("count", r.items.len().into())])
            }
        } else {
            String::new()
        };
        let list = items
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let color = match it.kind {
                    DiffKind::Added => theme.success,
                    DiffKind::Removed => theme.danger,
                    DiffKind::Changed => theme.warning,
                };
                h_flex()
                    .id(("compare-item", i))
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .when(self.selected == Some(i), |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
                    .child(
                        div()
                            .w(px(14.))
                            .text_color(color)
                            .child(kind_label(it.kind)),
                    )
                    .child(icons::object_icon(it.object_kind).xsmall())
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(it.name.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(it.detail.clone()),
                    )
            })
            .collect::<Vec<_>>();
        let has_script = !script.trim().is_empty();
        v_flex()
            .id("schema-compare")
            .test_support()
            .size_full()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().text_sm().child(t("compare-source")))
                    .child(src_picker)
                    .child(icons::Icon::from(icons::Lucide::ArrowRight).small())
                    .child(div().text_sm().child(t("compare-target")))
                    .child(tgt_picker)
                    .child(div().flex_1())
                    .child(
                        Button::new("compare-run")
                            .primary()
                            .small()
                            .label(t("compare-run"))
                            .disabled(self.running)
                            .on_click(cx.listener(|this, _, _, cx| this.compare(cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .text_sm()
                    .child(
                        Checkbox::new("compare-drop")
                            .label(t("compare-drop-removed"))
                            .checked(self.opts.drop_removed)
                            .on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.opts.drop_removed = *v;
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("compare-comments")
                            .label(t("compare-ignore-comments"))
                            .checked(self.opts.ignore_comments)
                            .on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.opts.ignore_comments = *v;
                                cx.notify();
                            })),
                    )
                    .child(
                        Checkbox::new("compare-routines")
                            .label(t("compare-routines"))
                            .checked(self.opts.include_routines)
                            .on_click(cx.listener(|this, v: &bool, _, cx| {
                                this.opts.include_routines = *v;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    ),
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
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .child(
                        v_flex()
                            .id("compare-items")
                            .w(px(380.))
                            .h_full()
                            .border_1()
                            .border_color(theme.border)
                            .rounded_md()
                            .overflow_y_scroll()
                            .text_sm()
                            .children(list),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .border_1()
                            .border_color(theme.border)
                            .rounded_md()
                            .children(self.diff.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(t("compare-script")),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("compare-console")
                            .small()
                            .icon(icons::Lucide::SquareTerminal)
                            .label(t("compare-open-console"))
                            .disabled(!has_script)
                            .on_click(cx.listener(|this, _, w, cx| this.open_in_console(w, cx))),
                    )
                    .child(
                        Button::new("compare-apply")
                            .small()
                            .danger()
                            .label(t("compare-apply"))
                            .disabled(!has_script || self.running)
                            .on_click(cx.listener(|this, _, w, cx| this.apply(w, cx))),
                    ),
            )
            .child(
                div()
                    .id("compare-script")
                    .test_support()
                    .h(px(130.))
                    .p_2()
                    .rounded_md()
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .overflow_y_scroll()
                    .child(if has_script {
                        script
                    } else {
                        t("compare-no-script")
                    }),
            )
    }
}

/// Most recent comparison view (UI tests).
#[derive(Default)]
pub struct LastSchemaCompare(pub Option<Entity<SchemaCompareView>>);
impl Global for LastSchemaCompare {}

pub fn open(source: Option<NodeRef>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| SchemaCompareView::new(source, cx));
    cx.set_global(LastSchemaCompare(Some(view.clone())));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t("action-compare"))
            .w(px(1100.))
            .child(div().h(px(640.)).child(view.clone()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn containers_and_scripts() {
        let n = NodeRef::root("dev".into())
            .child(ObjectKind::Database, "shop")
            .child(ObjectKind::Schema, "public")
            .child(ObjectKind::Table, "orders");
        let c = container_of(&n);
        assert_eq!(c.name(), Some("public"));
        assert_eq!(label(&c), "shop / public");
        let s = split_script(
            "ALTER TABLE a ADD b int;\nCREATE TRIGGER t AFTER INSERT ON a BEGIN\n  UPDATE a SET b = 1;\nEND;\nDROP VIEW v;\n",
        );
        assert_eq!(s.len(), 3);
        assert!(s[1].starts_with("CREATE TRIGGER") && s[1].ends_with("END;"));
    }
}
