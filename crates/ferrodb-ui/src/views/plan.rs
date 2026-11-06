//! Execution plan view (plan 2.8): plan tree with cost bars.

use ferrodb_api::PlanNode;
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        ActiveTheme, h_flex,
        list::ListItem,
        tree::{Tree, TreeItem, TreeState},
        v_flex,
    },
};

use crate::i18n::t_args;

const SEP: char = '\u{1f}';

fn items(node: &PlanNode, path: &str) -> TreeItem {
    let label = format!(
        "{}{SEP}{}{SEP}{}{SEP}{}",
        node.operation,
        node.detail.clone().unwrap_or_default(),
        node.cost,
        node.rows
    );
    TreeItem::new(format!("plan:{path}"), label)
        .expanded(true)
        .children(
            node.children
                .iter()
                .enumerate()
                .map(|(i, c)| items(c, &format!("{path}.{i}"))),
        )
}

pub struct PlanView {
    pub plan: PlanNode,
    tree: Entity<TreeState>,
}

impl PlanView {
    pub fn new(plan: PlanNode, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = items(&plan, "0");
        let tree = cx.new(|cx| TreeState::new(cx).items(vec![root]));
        Self { plan, tree }
    }
}

impl Render for PlanView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let total = self.plan.cost.max(f64::EPSILON);
        let theme = cx.theme().clone();
        v_flex()
            .id("plan-view")
            .test_support()
            .size_full()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(t_args(
                        "explain-summary",
                        &[
                            ("cost", format!("{:.1}", self.plan.cost).into()),
                            ("rows", self.plan.rows.into()),
                        ],
                    )),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Tree::new(&self.tree, move |ix, entry, selected, _, _| {
                        let label = entry.item().label.clone();
                        let p: Vec<&str> = label.split(SEP).collect();
                        let cost: f64 = p.get(2).and_then(|c| c.parse().ok()).unwrap_or(0.0);
                        let share = (cost / total).clamp(0.0, 1.0) as f32;
                        ListItem::new(ix)
                            .selected(selected)
                            .pl(px(8. + 18. * entry.depth() as f32))
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(
                                        div().font_weight(FontWeight::SEMIBOLD).child(
                                            p.first().copied().unwrap_or_default().to_owned(),
                                        ),
                                    )
                                    .child(
                                        div().flex_1().text_color(theme.muted_foreground).child(
                                            p.get(1).copied().unwrap_or_default().to_owned(),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .w(px(120.))
                                            .h(px(6.))
                                            .rounded_sm()
                                            .bg(theme.muted)
                                            .child(
                                                div().h_full().rounded_sm().w(relative(share)).bg(
                                                    if share > 0.5 {
                                                        theme.warning
                                                    } else {
                                                        theme.primary
                                                    },
                                                ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .w(px(140.))
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!(
                                                "cost {cost:.1} · rows {}",
                                                p.get(3).copied().unwrap_or_default()
                                            )),
                                    ),
                            )
                    })
                    .size_full(),
                ),
            )
    }
}
