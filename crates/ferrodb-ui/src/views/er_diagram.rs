//! ER diagram (plan 2.8, mockup): tables of a schema as boxes with their
//! columns, foreign keys drawn as connectors.

use ferrodb_api::{DbObject, NodeRef, ObjectKind};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{ActiveTheme, Sizable as _, h_flex, v_flex},
};

use crate::{AppState, i18n::t_args};

const BOX_W: f32 = 200.;
const ROW_H: f32 = 18.;
const HEADER_H: f32 = 26.;
const MAX_COLS: usize = 8;
const PER_ROW: usize = 4;
const GAP_X: f32 = 60.;
const GAP_Y: f32 = 40.;
const MAX_TABLES: usize = 12;

#[derive(Clone, Debug, PartialEq)]
pub struct EntityBox {
    pub name: String,
    pub columns: Vec<(String, String)>,
    /// Target tables of foreign keys.
    pub references: Vec<String>,
    pub origin: Point<Pixels>,
}

impl EntityBox {
    fn height(&self) -> f32 {
        HEADER_H + ROW_H * self.columns.len().min(MAX_COLS) as f32 + 8.
    }
}

/// Target table of a foreign key detail (`→ customers(id)`).
pub fn fk_target(detail: &str) -> Option<String> {
    let s = detail.trim_start_matches('→').trim();
    let name = s.split('(').next()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Grid layout: rows of `PER_ROW` boxes.
pub fn layout(boxes: &mut [EntityBox]) {
    let mut y = 20.;
    for row in boxes.chunks_mut(PER_ROW) {
        let h = row.iter().map(EntityBox::height).fold(0., f32::max);
        for (i, b) in row.iter_mut().enumerate() {
            b.origin = point(px(20. + i as f32 * (BOX_W + GAP_X)), px(y));
        }
        y += h + GAP_Y;
    }
}

pub struct ErDiagram {
    pub title: String,
    pub boxes: Vec<EntityBox>,
    pub loading: bool,
}

impl ErDiagram {
    /// Diagram of a schema / dataset (or of the schema containing a table).
    pub fn new(node: NodeRef, cx: &mut Context<Self>) -> Self {
        let schema = if node.kind() == Some(ObjectKind::Table) {
            let mut p = node.clone();
            p.path.pop();
            p
        } else {
            node
        };
        let title = schema.name().unwrap_or_default().to_owned();
        let backend = AppState::backend(cx);
        let task = cx.background_executor().spawn(async move {
            let objs = backend
                .metadata()
                .children(schema)
                .await
                .unwrap_or_default();
            let tables: Vec<DbObject> = objs
                .into_iter()
                .filter(|o| o.kind == ObjectKind::Table)
                .take(MAX_TABLES)
                .collect();
            let mut boxes = Vec::new();
            for tbl in tables {
                let children = backend
                    .metadata()
                    .children(tbl.node.clone())
                    .await
                    .unwrap_or_default();
                boxes.push(EntityBox {
                    name: tbl.name.clone(),
                    columns: children
                        .iter()
                        .filter(|c| c.kind == ObjectKind::Column)
                        .map(|c| (c.name.clone(), c.detail.clone().unwrap_or_default()))
                        .collect(),
                    references: children
                        .iter()
                        .filter(|c| c.kind == ObjectKind::ForeignKey)
                        .filter_map(|c| c.detail.as_deref().and_then(fk_target))
                        .filter(|t| *t != tbl.name)
                        .collect(),
                    origin: Point::default(),
                });
            }
            layout(&mut boxes);
            boxes
        });
        cx.spawn(async move |this, cx| {
            let boxes = task.await;
            let _ = this.update(cx, |this, cx| {
                this.boxes = boxes;
                this.loading = false;
                cx.notify();
            });
        })
        .detach();
        Self {
            title,
            boxes: Vec::new(),
            loading: true,
        }
    }

    /// (from, to) connector end points in diagram coordinates.
    pub fn connectors(&self) -> Vec<(Point<Pixels>, Point<Pixels>)> {
        let mut out = Vec::new();
        for b in &self.boxes {
            for r in &b.references {
                if let Some(target) = self.boxes.iter().find(|x| &x.name == r) {
                    let from = point(b.origin.x + px(BOX_W), b.origin.y + px(HEADER_H / 2.));
                    let to = point(target.origin.x, target.origin.y + px(HEADER_H / 2.));
                    out.push((from, to));
                }
            }
        }
        out
    }
}

impl Render for ErDiagram {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let lines = self.connectors();
        let line_color = theme.primary;
        let boxes = self.boxes.iter().map(|b| {
            div()
                .absolute()
                .left(b.origin.x)
                .top(b.origin.y)
                .w(px(BOX_W))
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .shadow_sm()
                .child(
                    h_flex()
                        .h(px(HEADER_H))
                        .px_2()
                        .gap_1()
                        .bg(theme.muted)
                        .rounded_t_md()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(icons::object_icon(ObjectKind::Table).xsmall())
                        .child(b.name.clone()),
                )
                .child(v_flex().px_2().py_1().text_xs().children(
                    b.columns.iter().take(MAX_COLS).map(|(n, ty)| {
                        h_flex()
                            .h(px(ROW_H))
                            .gap_2()
                            .child(div().flex_1().child(n.clone()))
                            .child(div().text_color(theme.muted_foreground).child(ty.clone()))
                    }),
                ))
        });
        v_flex()
            .id("er-diagram")
            .test_support()
            .size_full()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(t_args(
                        "er-summary",
                        &[
                            ("schema", self.title.clone().into()),
                            ("tables", self.boxes.len().into()),
                            ("relations", lines.len().into()),
                        ],
                    )),
            )
            .child(
                div()
                    .id("er-canvas")
                    .flex_1()
                    .relative()
                    .overflow_scroll()
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                for (from, to) in &lines {
                                    let o = bounds.origin;
                                    let (a, b) = (o + *from, o + *to);
                                    let mid = (a.x + b.x) / 2.;
                                    let mut path = PathBuilder::stroke(px(1.5));
                                    path.move_to(a);
                                    path.line_to(point(mid, a.y));
                                    path.line_to(point(mid, b.y));
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn foreign_key_targets() {
        assert_eq!(fk_target("→ customers(id)").as_deref(), Some("customers"));
        assert_eq!(fk_target(""), None);
    }

    #[::core::prelude::v1::test]
    fn layout_wraps_rows() {
        let mut boxes: Vec<EntityBox> = (0..5)
            .map(|i| EntityBox {
                name: format!("t{i}"),
                columns: vec![("id".into(), "int".into())],
                references: vec![],
                origin: Point::default(),
            })
            .collect();
        layout(&mut boxes);
        assert_eq!(boxes[0].origin.y, boxes[3].origin.y);
        assert!(boxes[4].origin.y > boxes[0].origin.y);
        assert_eq!(boxes[4].origin.x, boxes[0].origin.x);
    }
}
