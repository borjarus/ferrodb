//! ER diagram (plan, phase 7): the tables of a schema from
//! [`MetadataService::er_model`] as boxes with their columns (primary and
//! foreign key columns marked), each foreign key drawn from its column to
//! the referenced column.
//!
//! Tables are placed in layers: referenced tables left of the tables that
//! reference them. Boxes can be dragged by their header; the positions are
//! saved in the project (`.ferrodb/diagrams/<source>-<schema>.json`, plan
//! 5.10) and restored the next time.
//!
//! [`MetadataService::er_model`]: ferrodb_api::MetadataService::er_model

use std::{collections::HashMap, path::PathBuf};

use ferrodb_api::{ErModel, NodeRef, ObjectKind};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{ActiveTheme, Sizable as _, h_flex, v_flex},
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState,
    i18n::{t, t_args},
};

pub const BOX_W: f32 = 220.;
pub const ROW_H: f32 = 18.;
pub const HEADER_H: f32 = 26.;
pub const MAX_COLS: usize = 14;
const GAP_X: f32 = 90.;
const GAP_Y: f32 = 30.;
const MARGIN: f32 = 20.;

#[derive(Clone, Debug, PartialEq)]
pub struct EntityBox {
    pub name: String,
    pub view: bool,
    /// (name, type, primary, foreign).
    pub columns: Vec<(String, String, bool, bool)>,
    pub origin: Point<Pixels>,
}

impl EntityBox {
    pub fn shown_columns(&self) -> usize {
        self.columns.len().min(MAX_COLS)
    }

    pub fn height(&self) -> f32 {
        let more = usize::from(self.columns.len() > MAX_COLS);
        HEADER_H + ROW_H * (self.shown_columns() + more) as f32 + 8.
    }

    /// Vertical center of a column row, relative to the box (header when
    /// the column is not shown).
    pub fn row_y(&self, column: &str) -> f32 {
        match self
            .columns
            .iter()
            .take(MAX_COLS)
            .position(|c| c.0 == column)
        {
            Some(i) => HEADER_H + 4. + ROW_H * i as f32 + ROW_H / 2.,
            None => HEADER_H / 2.,
        }
    }
}

/// Saved positions of a diagram.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedDiagram {
    pub container: String,
    pub positions: HashMap<String, (f32, f32)>,
}

/// Layer of each entity: 0 for tables referencing nothing (in the
/// diagram), else one more than the deepest table they reference.
pub fn layers(model: &ErModel) -> Vec<usize> {
    let idx: HashMap<&str, usize> = model
        .entities
        .iter()
        .enumerate()
        .map(|(i, e)| (e.name.as_str(), i))
        .collect();
    let mut layer = vec![0usize; model.entities.len()];
    // Longest path, bounded by the number of tables (cycles stop there).
    for _ in 0..model.entities.len() {
        let mut changed = false;
        for r in &model.relations {
            let (Some(&f), Some(&t)) = (idx.get(r.from.as_str()), idx.get(r.to.as_str())) else {
                continue;
            };
            if f != t && layer[f] < layer[t] + 1 && layer[t] + 1 < model.entities.len() {
                layer[f] = layer[t] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    layer
}

/// Boxes placed in layers (columns of the diagram), stacked top-down.
pub fn layout(model: &ErModel) -> Vec<EntityBox> {
    let layer = layers(model);
    let mut boxes: Vec<EntityBox> = model
        .entities
        .iter()
        .map(|e| EntityBox {
            name: e.name.clone(),
            view: e.kind == Some(ObjectKind::View),
            columns: e
                .columns
                .iter()
                .map(|c| (c.name.clone(), c.data_type.clone(), c.primary, c.foreign))
                .collect(),
            origin: Point::default(),
        })
        .collect();
    let mut y_per_layer: HashMap<usize, f32> = HashMap::new();
    for (i, b) in boxes.iter_mut().enumerate() {
        let l = layer[i];
        let y = y_per_layer.entry(l).or_insert(MARGIN);
        b.origin = point(px(MARGIN + l as f32 * (BOX_W + GAP_X)), px(*y));
        *y += b.height() + GAP_Y;
    }
    boxes
}

/// File name of a diagram in `.ferrodb/diagrams`.
pub fn diagram_file(node: &NodeRef) -> String {
    let mut parts = vec![node.data_source.as_str().to_owned()];
    parts.extend(node.path.iter().map(|(_, n)| n.clone()));
    let name: String = parts
        .join("-")
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("{name}.json")
}

/// A connector: from the referencing column of `from` to the referenced
/// column of `to`, as two points in diagram coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Connector {
    pub a: Point<Pixels>,
    pub b: Point<Pixels>,
    pub optional: bool,
}

struct Drag {
    index: usize,
    start_mouse: Point<Pixels>,
    start_origin: Point<Pixels>,
}

pub struct ErDiagram {
    pub title: String,
    pub container: NodeRef,
    pub model: ErModel,
    pub boxes: Vec<EntityBox>,
    pub loading: bool,
    pub error: Option<String>,
    /// Where the positions are saved; `None` = not saved.
    pub file: Option<PathBuf>,
    drag: Option<Drag>,
}

impl ErDiagram {
    /// Diagram of a schema / database (or of the schema containing a table).
    pub fn new(node: NodeRef, diagrams_dir: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let mut container = node.clone();
        while container
            .kind()
            .is_some_and(|k| !matches!(k, ObjectKind::Schema | ObjectKind::Database | ObjectKind::Keyspace | ObjectKind::Dataset))
        {
            container.path.pop();
        }
        let title = container.name().unwrap_or_default().to_owned();
        let file = diagrams_dir.map(|d| d.join(diagram_file(&container)));
        let backend = AppState::backend(cx);
        let target = container.clone();
        let saved_file = file.clone();
        let task = cx.background_executor().spawn(async move {
            let model = backend.metadata().er_model(target).await?;
            let saved: Option<SavedDiagram> = saved_file
                .and_then(|f| std::fs::read_to_string(f).ok())
                .and_then(|s| serde_json::from_str(&s).ok());
            Ok::<_, ferrodb_api::Error>((model, saved))
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok((model, saved)) => {
                        let mut boxes = layout(&model);
                        if let Some(saved) = saved {
                            for b in &mut boxes {
                                if let Some((x, y)) = saved.positions.get(&b.name) {
                                    b.origin = point(px(*x), px(*y));
                                }
                            }
                        }
                        this.model = model;
                        this.boxes = boxes;
                    }
                    Err(e) => this.error = Some(e.to_string()),
                }
                this.loading = false;
                cx.notify();
            });
        })
        .detach();
        Self {
            title,
            container,
            model: ErModel::default(),
            boxes: Vec::new(),
            loading: true,
            error: None,
            file,
            drag: None,
        }
    }

    fn find(&self, name: &str) -> Option<&EntityBox> {
        self.boxes.iter().find(|b| b.name == name)
    }

    /// Connector end points in diagram coordinates.
    pub fn connectors(&self) -> Vec<Connector> {
        let mut out = Vec::new();
        for r in &self.model.relations {
            let (Some(from), Some(to)) = (self.find(&r.from), self.find(&r.to)) else {
                continue;
            };
            let fy = from.origin.y + px(from.row_y(r.from_columns.first().map(String::as_str).unwrap_or("")));
            let ty = to.origin.y + px(to.row_y(r.to_columns.first().map(String::as_str).unwrap_or("")));
            // Leave from the side facing the other box.
            let (a, b) = if from.origin.x >= to.origin.x + px(BOX_W / 2.) {
                (point(from.origin.x, fy), point(to.origin.x + px(BOX_W), ty))
            } else if to.origin.x >= from.origin.x + px(BOX_W / 2.) {
                (point(from.origin.x + px(BOX_W), fy), point(to.origin.x, ty))
            } else {
                // Same column (or a self reference): loop on the right side.
                (point(from.origin.x + px(BOX_W), fy), point(to.origin.x + px(BOX_W), ty))
            };
            out.push(Connector {
                a,
                b,
                optional: r.optional,
            });
        }
        out
    }

    /// Moves box `index` to `origin` (dragging, tests).
    pub fn move_box(&mut self, index: usize, origin: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(b) = self.boxes.get_mut(index) {
            b.origin = point(origin.x.max(px(0.)), origin.y.max(px(0.)));
            cx.notify();
        }
    }

    /// Writes the positions to the project.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(f) = &self.file else { return Ok(()) };
        let saved = SavedDiagram {
            container: self.title.clone(),
            positions: self
                .boxes
                .iter()
                .map(|b| (b.name.clone(), (f32::from(b.origin.x), f32::from(b.origin.y))))
                .collect(),
        };
        if let Some(dir) = f.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(f, serde_json::to_string_pretty(&saved).unwrap_or_default())
    }

    /// Back to the computed layout (and forget the saved one).
    pub fn reset_layout(&mut self, cx: &mut Context<Self>) {
        self.boxes = layout(&self.model);
        if let Some(f) = &self.file {
            let _ = std::fs::remove_file(f);
        }
        cx.notify();
    }
}

impl Render for ErDiagram {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let lines = self.connectors();
        let line_color = theme.primary;
        let optional_color = theme.muted_foreground;
        let boxes = self.boxes.iter().enumerate().map(|(i, b)| {
            let more = b.columns.len().saturating_sub(MAX_COLS);
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
                        .id(("er-head", i))
                        .h(px(HEADER_H))
                        .px_2()
                        .gap_1()
                        .bg(theme.muted)
                        .rounded_t_md()
                        .cursor_move()
                        .font_weight(FontWeight::SEMIBOLD)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                                if let Some(b) = this.boxes.get(i) {
                                    this.drag = Some(Drag {
                                        index: i,
                                        start_mouse: ev.position,
                                        start_origin: b.origin,
                                    });
                                    cx.stop_propagation();
                                }
                            }),
                        )
                        .child(
                            icons::object_icon(if b.view {
                                ObjectKind::View
                            } else {
                                ObjectKind::Table
                            })
                            .xsmall(),
                        )
                        .child(b.name.clone()),
                )
                .child(
                    v_flex()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .children(b.columns.iter().take(MAX_COLS).map(|(n, ty, pk, fk)| {
                            h_flex()
                                .h(px(ROW_H))
                                .gap_1()
                                .child(
                                    div()
                                        .w(px(14.))
                                        .text_color(if *pk { theme.warning } else { theme.primary })
                                        .child(match (pk, fk) {
                                            (true, _) => "PK",
                                            (false, true) => "FK",
                                            _ => "",
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .when(*pk, |d| d.font_weight(FontWeight::SEMIBOLD))
                                        .child(n.clone()),
                                )
                                .child(
                                    div()
                                        .max_w(px(90.))
                                        .overflow_hidden()
                                        .text_color(theme.muted_foreground)
                                        .child(ty.clone()),
                                )
                        }))
                        .when(more > 0, |d| {
                            d.child(
                                div()
                                    .h(px(ROW_H))
                                    .text_color(theme.muted_foreground)
                                    .child(t_args("er-more-columns", &[("count", more.into())])),
                            )
                        }),
                )
        })
        .collect::<Vec<_>>();
        let width = self
            .boxes
            .iter()
            .map(|b| f32::from(b.origin.x) + BOX_W)
            .fold(0., f32::max)
            + MARGIN * 2.;
        let height = self
            .boxes
            .iter()
            .map(|b| f32::from(b.origin.y) + b.height())
            .fold(0., f32::max)
            + MARGIN * 2.;
        let status = if self.loading {
            t("er-loading")
        } else if let Some(e) = &self.error {
            e.clone()
        } else {
            t_args(
                "er-summary",
                &[
                    ("schema", self.title.clone().into()),
                    ("tables", self.boxes.len().into()),
                    ("relations", lines.len().into()),
                ],
            )
        };
        v_flex()
            .id("er-diagram")
            .test_support()
            .size_full()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(status),
                    )
                    .child(
                        ferrodb_ui_kit::kit::button::Button::new("er-reset")
                            .xsmall()
                            .label(t("er-reset-layout"))
                            .on_click(cx.listener(|this, _, _, cx| this.reset_layout(cx))),
                    ),
            )
            .child(
                div()
                    .id("er-canvas")
                    .flex_1()
                    .overflow_scroll()
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                        if let Some(d) = &this.drag {
                            let (i, o, m) = (d.index, d.start_origin, d.start_mouse);
                            this.move_box(i, o + (ev.position - m), cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, _| {
                            if this.drag.take().is_some()
                                && let Err(e) = this.save()
                            {
                                tracing::warn!(error = %e, "diagram layout not saved");
                            }
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .w(px(width))
                            .h(px(height))
                            .child(
                                canvas(
                                    |_, _, _| {},
                                    move |bounds, _, window, _| {
                                        for c in &lines {
                                            let o = bounds.origin;
                                            let (a, b) = (o + c.a, o + c.b);
                                            let out = if b.x >= a.x { px(24.) } else { px(-24.) };
                                            let same_side = (a.x - b.x).abs() < px(1.);
                                            let mut path = PathBuilder::stroke(px(1.5));
                                            path.move_to(a);
                                            if same_side {
                                                let x = a.x + px(28.);
                                                path.line_to(point(x, a.y));
                                                path.line_to(point(x, b.y));
                                            } else {
                                                let mid = (a.x + b.x) / 2.;
                                                let _ = out;
                                                path.line_to(point(mid, a.y));
                                                path.line_to(point(mid, b.y));
                                            }
                                            path.line_to(b);
                                            if let Ok(p) = path.build() {
                                                window.paint_path(
                                                    p,
                                                    if c.optional { optional_color } else { line_color },
                                                );
                                            }
                                            // The referenced end: a small bar.
                                            let mut tick = PathBuilder::stroke(px(2.));
                                            tick.move_to(point(b.x, b.y - px(5.)));
                                            tick.line_to(point(b.x, b.y + px(5.)));
                                            if let Ok(p) = tick.build() {
                                                window.paint_path(p, line_color);
                                            }
                                        }
                                    },
                                )
                                .absolute()
                                .size_full(),
                            )
                            .children(boxes),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use ferrodb_api::{ErColumn, ErEntity, ErRelation};

    use super::*;

    fn entity(name: &str, cols: &[&str]) -> ErEntity {
        ErEntity {
            name: name.into(),
            kind: Some(ObjectKind::Table),
            columns: cols
                .iter()
                .map(|c| ErColumn {
                    name: (*c).into(),
                    data_type: "int".into(),
                    primary: *c == "id",
                    foreign: c.ends_with("_id"),
                    nullable: true,
                })
                .collect(),
            comment: None,
        }
    }

    fn rel(from: &str, col: &str, to: &str) -> ErRelation {
        ErRelation {
            from: from.into(),
            from_columns: vec![col.into()],
            to: to.into(),
            to_columns: vec!["id".into()],
            ..Default::default()
        }
    }

    fn model() -> ErModel {
        ErModel {
            container: "public".into(),
            entities: vec![
                entity("items", &["id", "order_id", "sku"]),
                entity("orders", &["id", "customer_id"]),
                entity("customers", &["id", "email"]),
                entity("tags", &["id"]),
            ],
            relations: vec![rel("items", "order_id", "orders"), rel("orders", "customer_id", "customers")],
        }
    }

    #[::core::prelude::v1::test]
    fn referenced_tables_come_first() {
        let m = model();
        assert_eq!(layers(&m), vec![2, 1, 0, 0]);
        let boxes = layout(&m);
        assert!(boxes[2].origin.x < boxes[1].origin.x && boxes[1].origin.x < boxes[0].origin.x);
        // Two tables in layer 0 are stacked.
        assert_eq!(boxes[2].origin.x, boxes[3].origin.x);
        assert!(boxes[3].origin.y > boxes[2].origin.y);
    }

    #[::core::prelude::v1::test]
    fn cycles_terminate() {
        let mut m = model();
        m.relations.push(rel("customers", "id", "items"));
        let l = layers(&m);
        assert!(l.iter().all(|x| *x < m.entities.len()));
    }

    #[::core::prelude::v1::test]
    fn rows_and_files() {
        let b = &layout(&model())[0];
        assert_eq!(b.row_y("order_id"), HEADER_H + 4. + ROW_H * 1.5);
        assert_eq!(b.row_y("missing"), HEADER_H / 2.);
        let n = NodeRef::root("pg dev".into()).child(ObjectKind::Schema, "public");
        assert_eq!(diagram_file(&n), "pg_dev-public.json");
    }
}
