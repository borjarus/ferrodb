//! Component gallery (plan 2.9): every phase-2 component on mock data, in
//! both themes, with the mock scenarios at hand. Hosted as a dock panel
//! (Developer menu) and by the standalone `ferrodb-gallery` app.

use std::collections::HashMap;

use ferrodb_api::{DataSourceId, NodeRef, ObjectKind, PlanNode};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _,
        button::Button,
        dock::{BasePanel, Panel},
        h_flex,
        list::ListItem,
        v_flex,
    },
    theme::{Appearance, is_dark},
};

use super::{ConsolePanel, ExplorerPanel, console::RunTarget, panel_boilerplate};
use crate::{
    AppState,
    dialogs::{data_source::DataSourceForm, driver_manager::DriverManager},
    i18n::t,
    settings_view,
    views::{
        diff::DiffView,
        er_diagram::ErDiagram,
        plan::PlanView,
        table_editor::TableEditor,
        wizard::{Wizard, WizardKind},
    },
};

pub const NAME: &str = "Gallery";

/// Gallery sections: (message id, builder).
pub const SECTIONS: [&str; 11] = [
    "panel-explorer",
    "gallery-grid",
    "gallery-documents",
    "gallery-keys",
    "action-new-data-source",
    "action-driver-manager",
    "action-explain",
    "action-er-diagram",
    "action-new-table",
    "action-compare",
    "action-export-data",
];

fn sample_plan() -> PlanNode {
    let n = |op: &str, d: Option<&str>, cost, rows, children| PlanNode {
        operation: op.into(),
        detail: d.map(Into::into),
        cost,
        rows,
        children,
    };
    n(
        "Hash Join",
        Some("o.customer_id = c.id"),
        1830.0,
        12_400,
        vec![
            n("Seq Scan", Some("orders o"), 1120.0, 48_000, vec![]),
            n("Index Scan", Some("customers_pkey"), 380.0, 10_000, vec![]),
        ],
    )
}

fn console(ds: &str, text: &str, window: &mut Window, cx: &mut App) -> AnyView {
    let c = cx.new(|cx| ConsolePanel::new(Some(DataSourceId::new(ds)), text.into(), window, cx));
    c.update(cx, |c, cx| c.run(RunTarget::Script, window, cx));
    c.into()
}

/// Builds the view of section `ix`.
pub fn build_section(ix: usize, window: &mut Window, cx: &mut App) -> AnyView {
    let schema = NodeRef::root(DataSourceId::new("shop-dev"))
        .child(ObjectKind::Database, "shop")
        .child(ObjectKind::Schema, "public");
    match ix {
        0 => cx.new(|cx| ExplorerPanel::new(window, cx)).into(),
        1 => console("shop-dev", "-- customers\nSELECT * FROM customers", window, cx),
        2 => console("catalog", "db.customers.find({})", window, cx),
        3 => console("cache", "SCAN 0 MATCH app:* COUNT 500", window, cx),
        4 => cx.new(|cx| DataSourceForm::new(None, window, cx)).into(),
        5 => cx.new(DriverManager::new).into(),
        6 => cx.new(|cx| PlanView::new(sample_plan(), window, cx)).into(),
        7 => cx.new(|cx| ErDiagram::new(schema, cx)).into(),
        8 => cx.new(|cx| TableEditor::new(None, window, cx)).into(),
        9 => cx
            .new(|cx| {
                DiffView::new(
                    "shop (dev)".into(),
                    "CREATE TABLE customers (\n    id bigint,\n    name text,\n    city text\n);",
                    "shop (production)".into(),
                    "CREATE TABLE customers (\n    id bigint,\n    name varchar(200),\n    city text,\n    vip boolean\n);",
                    "sql",
                    cx,
                )
            })
            .into(),
        _ => cx.new(|cx| Wizard::new(WizardKind::Export, window, cx)).into(),
    }
}

pub struct GalleryPanel {
    focus: FocusHandle,
    pub active: usize,
    views: HashMap<usize, AnyView>,
}

impl GalleryPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            active: 0,
            views: HashMap::new(),
        }
    }

    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.active = ix.min(SECTIONS.len() - 1);
        cx.notify();
    }

    /// Builds every section (smoke test of all components).
    pub fn build_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for ix in 0..SECTIONS.len() {
            if let std::collections::hash_map::Entry::Vacant(e) = self.views.entry(ix) {
                e.insert(build_section(ix, window, cx));
            }
        }
    }

    pub fn built(&self) -> usize {
        self.views.len()
    }
}

panel_boilerplate!(GalleryPanel);

impl BasePanel for GalleryPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for GalleryPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-gallery")
    }
}

impl Render for GalleryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.views.contains_key(&self.active) {
            let v = build_section(self.active, window, cx);
            self.views.insert(self.active, v);
        }
        let view = self.views[&self.active].clone();
        let theme = cx.theme().clone();
        let scenarios = AppState::global(cx).scenarios.clone();
        let nav = v_flex()
            .w(px(200.))
            .h_full()
            .gap_0p5()
            .p_1()
            .border_r_1()
            .border_color(theme.border)
            .children(SECTIONS.iter().enumerate().map(|(i, id)| {
                ListItem::new(("gallery-section", i))
                    .selected(i == self.active)
                    .child(t(id))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(i, cx)))
            }))
            .child(div().flex_1())
            .child(
                Button::new("gallery-theme")
                    .xsmall()
                    .icon(if is_dark(cx) {
                        icons::Lucide::Sun
                    } else {
                        icons::Lucide::Moon
                    })
                    .label(t("action-toggle-theme"))
                    .on_click(|_, _, cx| {
                        let next = if is_dark(cx) {
                            Appearance::Light
                        } else {
                            Appearance::Dark
                        };
                        settings_view::change(cx, |s| s.appearance = next.id().into());
                    }),
            )
            .when_some(scenarios, |d, control| {
                let current = control.current();
                d.child(
                    div()
                        .pt_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("action-dev-scenarios")),
                )
                .children(control.scenarios().into_iter().map(|id| {
                    let control = control.clone();
                    ListItem::new(SharedString::from(format!("gallery-scenario-{id}")))
                        .selected(id == current)
                        .child(t(&format!("scenario-{id}")))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            control.set(id);
                            // Rebuild views so they pick up the new scenario.
                            this.views.clear();
                            cx.notify();
                        }))
                }))
            });
        h_flex()
            .id("gallery")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .child(nav)
            .child(div().flex_1().h_full().min_w_0().p_2().child(view))
    }
}
