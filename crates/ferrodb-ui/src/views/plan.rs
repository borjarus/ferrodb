//! Execution plan view (plan, phase 7): one [`ExecutionPlan`] shown as a
//! tree, a table (one row per operation with self cost and time) or a
//! diagram (operations as boxes, inputs below, shaded by their share of the
//! cost or time). Warnings of the plan are listed above; the selected
//! operation's properties are shown on the right.

use ferrodb_api::{ExecutionPlan, PlanNode};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        list::ListItem,
        tab::{Tab, TabBar},
        tree::{Tree, TreeItem, TreeState},
        v_flex,
    },
};

use crate::i18n::{t, t_args};

const SEP: char = '\u{1f}';

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanMode {
    Tree,
    Table,
    Diagram,
}

/// Box size and gaps of the diagram.
pub const BOX_W: f32 = 190.;
pub const BOX_H: f32 = 58.;
const GAP_X: f32 = 24.;
const GAP_Y: f32 = 36.;

/// Diagram positions (top-left corners) of the nodes in `flatten()` order:
/// leaves side by side, each parent centered over its inputs, the root on
/// top.
pub fn diagram_layout(root: &PlanNode) -> Vec<(f32, f32)> {
    fn place(n: &PlanNode, depth: usize, next_x: &mut f32, out: &mut Vec<(f32, f32)>) -> f32 {
        let me = out.len();
        out.push((0., depth as f32 * (BOX_H + GAP_Y)));
        let x = if n.children.is_empty() {
            let x = *next_x;
            *next_x += BOX_W + GAP_X;
            x
        } else {
            let xs: Vec<f32> = n
                .children
                .iter()
                .map(|c| place(c, depth + 1, next_x, out))
                .collect();
            (xs[0] + xs[xs.len() - 1]) / 2.
        };
        out[me].0 = x;
        x
    }
    let mut out = Vec::new();
    let mut next = 0.;
    place(root, 0, &mut next, &mut out);
    out
}

/// Parent index (in `flatten()` order) of every node; `None` for the root.
pub fn parents(root: &PlanNode) -> Vec<Option<usize>> {
    fn walk(n: &PlanNode, parent: Option<usize>, out: &mut Vec<Option<usize>>) {
        let me = out.len();
        out.push(parent);
        for c in &n.children {
            walk(c, Some(me), out);
        }
    }
    let mut out = Vec::new();
    walk(root, None, &mut out);
    out
}

fn fmt_ms(ms: f64) -> String {
    if ms >= 1000. {
        format!("{:.2} s", ms / 1000.)
    } else if ms >= 10. {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

fn fmt_num(n: f64) -> String {
    if n >= 1_000_000. {
        format!("{:.1}M", n / 1_000_000.)
    } else if n >= 10_000. {
        format!("{:.0}k", n / 1000.)
    } else if n.fract() == 0. {
        format!("{n:.0}")
    } else {
        format!("{n:.1}")
    }
}

fn tree_items(node: &PlanNode, path: &str, index: &mut usize) -> TreeItem {
    let ix = *index;
    *index += 1;
    let label = format!(
        "{}{SEP}{}{SEP}{ix}",
        node.operation,
        node.detail.clone().unwrap_or_default()
    );
    TreeItem::new(format!("plan:{path}"), label)
        .expanded(true)
        .children(
            node.children
                .iter()
                .enumerate()
                .map(|(i, c)| tree_items(c, &format!("{path}.{i}"), index))
                .collect::<Vec<_>>(),
        )
}

pub struct PlanView {
    pub plan: ExecutionPlan,
    pub mode: PlanMode,
    /// Selected operation (index in `flatten()` order).
    pub selected: usize,
    tree: Entity<TreeState>,
    /// Share (0–1) of the plan's total per node: self time when analyzed,
    /// self cost otherwise.
    weights: Vec<f32>,
}

impl PlanView {
    pub fn new(plan: ExecutionPlan, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut ix = 0;
        let root = tree_items(&plan.root, "0", &mut ix);
        let tree = cx.new(|cx| TreeState::new(cx).items(vec![root]));
        let nodes = plan.root.flatten();
        let own: Vec<f64> = nodes
            .iter()
            .map(|(_, n)| {
                if plan.analyzed {
                    n.self_ms().unwrap_or(0.)
                } else {
                    n.self_cost()
                }
            })
            .collect();
        let total: f64 = own.iter().sum::<f64>().max(f64::EPSILON);
        let weights = own.iter().map(|w| (w / total) as f32).collect();
        Self {
            plan,
            mode: PlanMode::Tree,
            selected: 0,
            tree,
            weights,
        }
    }

    pub fn set_mode(&mut self, mode: PlanMode, cx: &mut Context<Self>) {
        self.mode = mode;
        cx.notify();
    }

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = ix;
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let p = &self.plan;
        let mut parts = vec![t_args(
            "explain-summary",
            &[
                ("cost", fmt_num(p.root.cost).into()),
                ("rows", p.root.rows.into()),
            ],
        )];
        if let Some(ms) = p.planning_ms {
            parts.push(t_args("explain-planning", &[("time", fmt_ms(ms).into())]));
        }
        if let Some(ms) = p.execution_ms {
            parts.push(t_args("explain-execution", &[("time", fmt_ms(ms).into())]));
        }
        if p.analyzed {
            parts.push(t("explain-analyzed"));
        }
        let mode_ix = match self.mode {
            PlanMode::Tree => 0,
            PlanMode::Table => 1,
            PlanMode::Diagram => 2,
        };
        let raw = p.raw.clone();
        h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .child(
                TabBar::new("plan-mode")
                    .xsmall()
                    .selected_index(mode_ix)
                    .child(Tab::new().label(t("explain-mode-tree")))
                    .child(Tab::new().label(t("explain-mode-table")))
                    .child(Tab::new().label(t("explain-mode-diagram")))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        let mode = [PlanMode::Tree, PlanMode::Table, PlanMode::Diagram][*ix];
                        this.set_mode(mode, cx)
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(parts.join(" · ")),
            )
            .child(
                Button::new("plan-copy-raw")
                    .ghost()
                    .xsmall()
                    .icon(icons::Lucide::Copy)
                    .label(t("explain-copy-raw"))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(raw.clone()))
                    }),
            )
            .into_any_element()
    }

    fn warnings(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let w = self.plan.warnings();
        if w.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        Some(
            v_flex()
                .id("plan-warnings")
                .px_2()
                .py_1()
                .gap_0p5()
                .max_h(px(96.))
                .overflow_y_scroll()
                .bg(theme.warning.opacity(0.08))
                .text_xs()
                .children(w.into_iter().map(move |(op, msg)| {
                    h_flex()
                        .gap_1()
                        .child(
                            icons::Icon::from(icons::Lucide::TriangleAlert)
                                .xsmall()
                                .text_color(theme.warning),
                        )
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(op))
                        .child(msg)
                }))
                .into_any_element(),
        )
    }

    fn tree(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let nodes: Vec<PlanNode> = self
            .plan
            .root
            .flatten()
            .into_iter()
            .map(|(_, n)| PlanNode {
                children: Vec::new(),
                ..n.clone()
            })
            .collect();
        let weights = self.weights.clone();
        let analyzed = self.plan.analyzed;
        let view = cx.entity().downgrade();
        Tree::new(&self.tree, move |ix, entry, selected, _, _| {
            let label = entry.item().label.clone();
            let p: Vec<&str> = label.split(SEP).collect();
            let i: usize = p.get(2).and_then(|x| x.parse().ok()).unwrap_or(0);
            let n = &nodes[i.min(nodes.len().saturating_sub(1))];
            let share = weights.get(i).copied().unwrap_or(0.);
            let stats = if analyzed {
                format!(
                    "{} · rows {} / est. {}",
                    n.actual_ms.map(fmt_ms).unwrap_or_default(),
                    n.actual_rows.map(|r| r.to_string()).unwrap_or("–".into()),
                    n.rows
                )
            } else {
                format!("cost {} · rows {}", fmt_num(n.cost), n.rows)
            };
            let view = view.clone();
            ListItem::new(ix)
                .selected(selected)
                .pl(px(8. + 18. * entry.depth() as f32))
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |v, cx| v.select(i, cx));
                })
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .when(!n.warnings.is_empty(), |d| {
                            d.child(
                                icons::Icon::from(icons::Lucide::TriangleAlert)
                                    .xsmall()
                                    .text_color(theme.warning),
                            )
                        })
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(n.operation.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .overflow_hidden()
                                .text_color(theme.muted_foreground)
                                .child(p.get(1).copied().unwrap_or_default().to_owned()),
                        )
                        .child(bar(&theme, share))
                        .child(
                            div()
                                .w(px(220.))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(stats),
                        ),
                )
        })
        .size_full()
        .into_any_element()
    }

    fn table(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let analyzed = self.plan.analyzed;
        let head = |s: String, w: f32| {
            div()
                .w(px(w))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .child(s)
        };
        let header = h_flex()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(head("#".into(), 28.))
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("explain-col-operation")),
            )
            .child(head(t("explain-col-object"), 140.))
            .child(head(t("explain-col-cost"), 80.))
            .child(head(t("explain-col-self"), 80.))
            .child(head(t("explain-col-rows"), 80.))
            .when(analyzed, |d| {
                d.child(head(t("explain-col-actual-rows"), 90.))
                    .child(head(t("explain-col-time"), 80.))
                    .child(head(t("explain-col-loops"), 50.))
            });
        let rows = self
            .plan
            .root
            .flatten()
            .into_iter()
            .enumerate()
            .map(|(i, (depth, n))| {
                let selected = i == self.selected;
                let share = self.weights.get(i).copied().unwrap_or(0.);
                let cell = |s: String, w: f32| div().w(px(w)).flex_shrink_0().text_xs().child(s);
                h_flex()
                    .id(("plan-row", i))
                    .px_2()
                    .py_0p5()
                    .when(selected, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
                    .child(cell(format!("{i}"), 28.))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .pl(px(12. * depth as f32))
                            .when(!n.warnings.is_empty(), |d| {
                                d.child(
                                    icons::Icon::from(icons::Lucide::TriangleAlert)
                                        .xsmall()
                                        .text_color(theme.warning),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(n.operation.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .overflow_hidden()
                                    .text_color(theme.muted_foreground)
                                    .child(n.detail.clone().unwrap_or_default()),
                            ),
                    )
                    .child(cell(n.object.clone().unwrap_or_default(), 140.))
                    .child(cell(fmt_num(n.cost), 80.))
                    .child(div().w(px(80.)).flex_shrink_0().child(bar(&theme, share)))
                    .child(cell(n.rows.to_string(), 80.))
                    .when(analyzed, |d| {
                        d.child(cell(
                            n.actual_rows.map(|r| r.to_string()).unwrap_or("–".into()),
                            90.,
                        ))
                        .child(cell(n.actual_ms.map(fmt_ms).unwrap_or_default(), 80.))
                        .child(cell(
                            n.loops.map(|l| l.to_string()).unwrap_or_default(),
                            50.,
                        ))
                    })
            })
            .collect::<Vec<_>>();
        v_flex()
            .id("plan-table")
            .test_support()
            .size_full()
            .child(header)
            .child(
                v_flex()
                    .id("plan-table-rows")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    fn diagram(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let nodes = self.plan.root.flatten();
        let pos = diagram_layout(&self.plan.root);
        let parent = parents(&self.plan.root);
        let lines: Vec<(Point<Pixels>, Point<Pixels>)> = parent
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let p = (*p)?;
                let (cx_, cy) = pos[i];
                let (px_, py) = pos[p];
                Some((
                    point(px(px_ + BOX_W / 2. + 16.), px(py + BOX_H + 16.)),
                    point(px(cx_ + BOX_W / 2. + 16.), px(cy + 16.)),
                ))
            })
            .collect();
        let line_color = theme.border;
        let analyzed = self.plan.analyzed;
        let boxes = nodes.iter().enumerate().map(|(i, (_, n))| {
            let (x, y) = pos[i];
            let share = self.weights.get(i).copied().unwrap_or(0.);
            let hot = share > 0.3;
            let selected = i == self.selected;
            let stat = if analyzed {
                format!(
                    "{} · {} rows",
                    n.actual_ms.map(fmt_ms).unwrap_or_default(),
                    n.actual_rows.unwrap_or(0)
                )
            } else {
                format!("cost {} · {} rows", fmt_num(n.cost), n.rows)
            };
            div()
                .id(("plan-box", i))
                .absolute()
                .left(px(x + 16.))
                .top(px(y + 16.))
                .w(px(BOX_W))
                .h(px(BOX_H))
                .px_2()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(if selected {
                    theme.primary
                } else if !n.warnings.is_empty() {
                    theme.warning
                } else {
                    theme.border
                })
                .bg(if hot {
                    theme.warning.opacity(0.15 + share * 0.4)
                } else {
                    theme.background
                })
                .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .overflow_hidden()
                        .child(n.operation.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .overflow_hidden()
                        .text_color(theme.muted_foreground)
                        .child(n.object.clone().or(n.detail.clone()).unwrap_or_default()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(stat),
                )
        });
        let width = pos.iter().map(|p| p.0).fold(0., f32::max) + BOX_W + 48.;
        let height = pos.iter().map(|p| p.1).fold(0., f32::max) + BOX_H + 48.;
        div()
            .id("plan-diagram")
            .test_support()
            .size_full()
            .overflow_scroll()
            .child(
                div()
                    .relative()
                    .w(px(width))
                    .h(px(height))
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                for (a, b) in &lines {
                                    let o = bounds.origin;
                                    let (a, b) = (o + *a, o + *b);
                                    let mid = (a.y + b.y) / 2.;
                                    let mut path = PathBuilder::stroke(px(1.5));
                                    path.move_to(a);
                                    path.line_to(point(a.x, mid));
                                    path.line_to(point(b.x, mid));
                                    path.line_to(b);
                                    if let Ok(p) = path.build() {
                                        window.paint_path(p, line_color);
                                    }
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(boxes),
            )
            .into_any_element()
    }

    fn details(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let nodes = self.plan.root.flatten();
        let Some((_, n)) = nodes.get(self.selected) else {
            return div().into_any_element();
        };
        let row = |k: String, v: String| {
            h_flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .w(px(110.))
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child(k),
                )
                .child(div().flex_1().min_w_0().child(v))
        };
        let mut items = vec![
            row(
                t("explain-col-cost"),
                format!("{} (self {})", fmt_num(n.cost), fmt_num(n.self_cost())),
            ),
            row(t("explain-col-rows"), n.rows.to_string()),
        ];
        if let Some(r) = n.actual_rows {
            items.push(row(t("explain-col-actual-rows"), r.to_string()));
        }
        if let Some(ms) = n.actual_ms {
            items.push(row(
                t("explain-col-time"),
                format!(
                    "{} (self {})",
                    fmt_ms(ms),
                    n.self_ms().map(fmt_ms).unwrap_or_default()
                ),
            ));
        }
        if let Some(l) = n.loops {
            items.push(row(t("explain-col-loops"), l.to_string()));
        }
        if let Some(o) = &n.object {
            items.push(row(t("explain-col-object"), o.clone()));
        }
        for (k, v) in &n.properties {
            items.push(row(k.clone(), v.clone()));
        }
        v_flex()
            .id("plan-details")
            .test_support()
            .w(px(300.))
            .h_full()
            .flex_shrink_0()
            .p_2()
            .gap_1()
            .border_l_1()
            .border_color(theme.border)
            .overflow_y_scroll()
            .text_xs()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(n.operation.clone()),
            )
            .when_some(n.detail.clone(), |d, x| {
                d.child(div().text_color(theme.muted_foreground).child(x))
            })
            .children(n.warnings.iter().map(|w| {
                h_flex()
                    .gap_1()
                    .text_color(theme.warning)
                    .child(icons::Icon::from(icons::Lucide::TriangleAlert).xsmall())
                    .child(w.clone())
            }))
            .children(items)
            .into_any_element()
    }
}

/// A horizontal bar showing a 0–1 share.
fn bar(theme: &ferrodb_ui_kit::kit::Theme, share: f32) -> impl IntoElement {
    div()
        .w(px(70.))
        .h(px(6.))
        .rounded_sm()
        .bg(theme.muted)
        .child(
            div()
                .h_full()
                .rounded_sm()
                .w(relative(share.clamp(0., 1.)))
                .bg(if share > 0.3 {
                    theme.warning
                } else {
                    theme.primary
                }),
        )
}

impl Render for PlanView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.mode {
            PlanMode::Tree => self.tree(cx),
            PlanMode::Table => self.table(cx),
            PlanMode::Diagram => self.diagram(cx),
        };
        let header = self.header(cx);
        let warnings = self.warnings(cx);
        let details = self.details(cx);
        v_flex()
            .id("plan-view")
            .test_support()
            .size_full()
            .child(header)
            .children(warnings)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_1().min_w_0().h_full().child(body))
                    .child(details),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(op: &str, children: Vec<PlanNode>) -> PlanNode {
        PlanNode {
            operation: op.into(),
            children,
            ..Default::default()
        }
    }

    #[::core::prelude::v1::test]
    fn diagram_centers_parents_over_inputs() {
        let root = n(
            "join",
            vec![n("scan a", vec![]), n("hash", vec![n("scan b", vec![])])],
        );
        let pos = diagram_layout(&root);
        assert_eq!(pos.len(), 4);
        // Leaves side by side, depth by depth.
        assert_eq!(pos[1].0, 0.);
        assert_eq!(pos[3].0, BOX_W + GAP_X);
        assert_eq!(pos[2].0, pos[3].0);
        assert_eq!(pos[0].0, (pos[1].0 + pos[2].0) / 2.);
        assert!(pos[3].1 > pos[2].1 && pos[2].1 > pos[0].1);
        assert_eq!(parents(&root), vec![None, Some(0), Some(0), Some(2)]);
    }

    #[::core::prelude::v1::test]
    fn numbers_and_times_are_short() {
        assert_eq!(fmt_num(1834.5), "1834.5");
        assert_eq!(fmt_num(48_000.), "48k");
        assert_eq!(fmt_ms(0.123), "0.12 ms");
        assert_eq!(fmt_ms(2500.), "2.50 s");
    }
}
