//! Phase 7 flows on the deterministic mock: execution plan view modes,
//! `EXPLAIN ANALYZE`, the ER diagram on real relations (drag + saved
//! layout), schema comparison, the table editor producing `ALTER`, the
//! project files panel and Local History.

use std::{sync::Arc, time::Duration};

use ferrodb_api::{DataSourceId, NodeRef, ObjectKind};
use ferrodb_mock::MockBackend;
use ferrodb_ui::{
    AppState, Workspace, actions,
    panels::{ConsolePanel, FilesPanel, LocalHistoryPanel, console::ConsoleTab},
    project::Project,
    views::{
        er_diagram::ErDiagram,
        plan::{PlanMode, PlanView},
        schema_compare::LastSchemaCompare,
        table_editor::LastTableEditor,
    },
    workspace::LastViewDialog,
};
use ferrodb_ui_kit::kit::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Entity, Point, TestAppContext, Window, WindowBounds,
    WindowHandle, WindowOptions, base::Root, point, px, size, test::TestWindowExt as _,
};

struct Harness {
    window: WindowHandle<Root>,
    ws: Entity<Workspace>,
    dir: tempfile::TempDir,
}

fn start(cx: &mut TestAppContext) -> Harness {
    let mock = MockBackend::deterministic();
    cx.update(|cx: &mut App| {
        ferrodb_ui::init(
            AppState {
                backend: Arc::new(mock),
                scenarios: None,
                paths: None,
            },
            cx,
        );
        cx.set_reduce_motion(true);
    });
    let dir = tempfile::tempdir().unwrap();
    let project = Project::open(dir.path()).unwrap();
    let (window, ws) = cx.update(|cx| {
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(1400.), px(900.)),
        };
        let (window, ws) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Workspace::new(project, window, cx)),
        )
        .expect("open window");
        (window.downcast::<Root>().expect("root"), ws)
    });
    let h = Harness { window, ws, dir };
    h.frame(cx);
    h
}

impl Harness {
    fn with<R>(&self, cx: &mut TestAppContext, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        let r = cx
            .update_window(self.window.into(), |_, window, cx| f(window, cx))
            .unwrap();
        cx.run_until_parked();
        r
    }

    fn frame(&self, cx: &mut TestAppContext) {
        self.with(cx, |w, cx| w.render_frame(cx));
    }

    /// Lets debounced work (previews, saves) run.
    fn wait(&self, cx: &mut TestAppContext, ms: u64) {
        cx.executor().advance_clock(Duration::from_millis(ms));
        cx.run_until_parked();
    }

    fn console(&self, cx: &mut TestAppContext, ds: &str, text: &str) -> Entity<ConsolePanel> {
        let ws = self.ws.clone();
        let (ds, text) = (ds.to_owned(), text.to_owned());
        self.with(cx, |w, cx| {
            ws.update(cx, |ws, cx| {
                ws.add_console(Some(DataSourceId::new(ds)), text, w, cx)
            })
        })
    }

    fn plan(&self, cx: &mut TestAppContext, console: &Entity<ConsolePanel>) -> Entity<PlanView> {
        cx.update(|cx| {
            console
                .read(cx)
                .tabs
                .iter()
                .rev()
                .find_map(|t| match t {
                    ConsoleTab::Plan { view, .. } => Some(view.clone()),
                    _ => None,
                })
                .expect("a plan tab")
        })
    }
}

fn public(ds: &str) -> NodeRef {
    NodeRef::root(DataSourceId::new(ds))
        .child(ObjectKind::Database, "shop")
        .child(ObjectKind::Schema, "public")
}

#[gpui_kit::test]
async fn plan_modes_and_explain_analyze(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.console(
        cx,
        "shop-dev",
        "select * from orders o join customers c on c.id = o.customer_id",
    );
    h.with(cx, |w, cx| console.update(cx, |c, cx| c.explain(w, cx)));
    let plan = h.plan(cx, &console);
    cx.update(|cx| {
        let p = plan.read(cx);
        assert!(!p.plan.analyzed);
        assert_eq!(p.plan.root.operation, "Limit");
        assert_eq!(p.plan.warnings().len(), 1, "the 48k-row scan is flagged");
    });
    for mode in [PlanMode::Table, PlanMode::Diagram, PlanMode::Tree] {
        h.with(cx, |_, cx| plan.update(cx, |p, cx| p.set_mode(mode, cx)));
        h.frame(cx);
        let id = match mode {
            PlanMode::Table => "plan-table",
            PlanMode::Diagram => "plan-diagram",
            PlanMode::Tree => "plan-view",
        };
        h.with(cx, |w, _| assert!(w.find(id).visible(), "{id}"));
    }
    h.with(cx, |_, cx| plan.update(cx, |p, cx| p.select(3, cx)));
    h.frame(cx);
    h.with(cx, |w, _| assert!(w.find("plan-details").visible()));
    // Explain Analyze adds a second plan with actual numbers.
    h.with(cx, |w, cx| {
        console.update(cx, |c, cx| c.explain_analyze(w, cx))
    });
    let analyzed = h.plan(cx, &console);
    cx.update(|cx| {
        let p = analyzed.read(cx);
        assert!(p.plan.analyzed);
        assert!(p.plan.root.actual_ms.is_some());
        assert!(p.plan.statement.starts_with("EXPLAIN ANALYZE"));
    });
}

#[gpui_kit::test]
async fn er_diagram_draws_relations_and_keeps_moved_boxes(cx: &mut TestAppContext) {
    let h = start(cx);
    h.with(cx, |w, cx| {
        ferrodb_ui::workspace::open_er_diagram(public("shop-dev"), w, cx)
    });
    h.frame(cx);
    let view: Entity<ErDiagram> = cx.update(|cx| {
        cx.global::<LastViewDialog>()
            .0
            .clone()
            .expect("dialog view")
            .downcast()
            .expect("an ER diagram")
    });
    let (tables, lines, file) = cx.update(|cx| {
        let v = view.read(cx);
        (v.boxes.len(), v.connectors().len(), v.file.clone())
    });
    assert!(tables >= 5, "{tables}");
    assert!(
        lines >= 4,
        "every table but customers references customers: {lines}"
    );
    let file = file.expect("saved in the project");
    assert!(file.starts_with(h.dir.path().join(".ferrodb").join("diagrams")));
    // Move a box and save: the next diagram opens with it there.
    h.with(cx, |_, cx| {
        view.update(cx, |v, cx| {
            v.move_box(0, point(px(700.), px(400.)), cx);
            v.save().unwrap();
        })
    });
    h.with(cx, |w, cx| w.close_dialog(cx));
    h.with(cx, |w, cx| {
        ferrodb_ui::workspace::open_er_diagram(public("shop-dev"), w, cx)
    });
    h.frame(cx);
    let again: Entity<ErDiagram> = cx.update(|cx| {
        cx.global::<LastViewDialog>()
            .0
            .clone()
            .unwrap()
            .downcast()
            .unwrap()
    });
    cx.update(|cx| {
        let b = &again.read(cx).boxes[0];
        assert_eq!(b.origin, point(px(700.), px(400.)));
    });
    h.with(cx, |w, _| assert!(w.find("er-diagram").visible()));
}

#[gpui_kit::test]
async fn schema_comparison_dev_vs_prod(cx: &mut TestAppContext) {
    let h = start(cx);
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::CompareSchemas), cx)
    });
    h.frame(cx);
    let view = cx.update(|cx| {
        cx.global::<LastSchemaCompare>()
            .0
            .clone()
            .expect("compare view")
    });
    cx.update(|cx| {
        let v = view.read(cx);
        assert_eq!(
            v.source.as_ref().map(|n| n.data_source.as_str()),
            Some("shop-dev")
        );
        assert_eq!(
            v.target.as_ref().map(|n| n.data_source.as_str()),
            Some("shop-prod")
        );
    });
    h.with(cx, |_, cx| view.update(cx, |v, cx| v.compare(cx)));
    h.wait(cx, 200);
    h.frame(cx);
    cx.update(|cx| {
        let v = view.read(cx);
        let r = v.result.as_ref().expect("a comparison");
        assert!(!r.items.is_empty());
        assert!(r.script.contains("ADD COLUMN"), "{}", r.script);
    });
    h.with(cx, |_, cx| view.update(cx, |v, cx| v.select(0, cx)));
    h.frame(cx);
    h.with(cx, |w, _| {
        assert!(w.find("schema-compare").visible());
        assert!(w.find("compare-script").visible());
    });
}

#[gpui_kit::test]
async fn table_editor_generates_alter_for_a_table(cx: &mut TestAppContext) {
    let h = start(cx);
    let orders = public("shop-dev").child(ObjectKind::Table, "orders");
    h.with(cx, |w, cx| {
        ferrodb_ui::workspace::open_table_editor(Some(orders), w, cx)
    });
    h.wait(cx, 500);
    let editor = cx.update(|cx| cx.global::<LastTableEditor>().0.clone().expect("editor"));
    cx.update(|cx| {
        let e = editor.read(cx);
        assert!(e.columns.len() > 3, "design loaded");
        assert_eq!(e.foreign_keys.len(), 1);
        assert_eq!(e.ddl(), ferrodb_ui::i18n::t("table-editor-no-changes"));
    });
    // Change a type and add a column: ALTER statements in the preview.
    h.with(cx, |w, cx| {
        editor.update(cx, |e, cx| {
            let ty = e.columns[2].ty.clone();
            ty.update(cx, |i, cx| i.set_value("varchar(200)", w, cx));
            e.add_column("status", "text", true, false, w, cx);
        })
    });
    h.wait(cx, 500);
    cx.update(|cx| {
        let ddl = editor.read(cx).ddl();
        assert!(
            ddl.contains("ALTER COLUMN") && ddl.contains("TYPE varchar(200)"),
            "{ddl}"
        );
        assert!(ddl.contains("ADD COLUMN status text"), "{ddl}");
    });
    h.frame(cx);
    h.with(cx, |w, _| assert!(w.find("te-ddl").visible()));
    // A new table in the schema of the selection.
    h.with(cx, |w, cx| {
        ferrodb_ui::workspace::open_table_editor(Some(public("shop-dev")), w, cx)
    });
    h.wait(cx, 500);
    let fresh = cx.update(|cx| cx.global::<LastTableEditor>().0.clone().unwrap());
    cx.update(|cx| {
        let ddl = fresh.read(cx).ddl();
        assert!(ddl.starts_with("CREATE TABLE public.new_table"), "{ddl}");
    });
}

#[gpui_kit::test]
async fn project_files_open_scripts_in_bound_consoles(cx: &mut TestAppContext) {
    let h = start(cx);
    let sql = h.dir.path().join("reports").join("top.sql");
    std::fs::create_dir_all(sql.parent().unwrap()).unwrap();
    std::fs::write(&sql, "select 1").unwrap();
    std::fs::write(h.dir.path().join("orders.csv"), "id\n1\n").unwrap();
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::ShowProjectFiles), cx)
    });
    h.frame(cx);
    let panel: Entity<FilesPanel> = h.with(cx, |w, cx| cx.new(|cx| FilesPanel::new(w, cx)));
    let listed = cx.update(|cx| panel.read(cx).listed());
    assert!(listed.contains(&sql), "{listed:?}");
    assert!(listed.iter().any(|p| p.ends_with("orders.csv")));
    assert!(
        !listed
            .iter()
            .any(|p| p.components().any(|c| c.as_os_str() == ".ferrodb"))
    );
    // Opening a script binds the console to the file; saving writes it.
    let sql2 = sql.clone();
    h.with(cx, |w, cx| FilesPanel::open(sql2, w, cx));
    let console = cx.update(|cx| {
        h.ws.read(cx)
            .consoles()
            .into_iter()
            .find(|c| c.read(cx).file.is_some())
            .expect("a file console")
    });
    h.with(cx, |w, cx| {
        console.update(cx, |c, cx| {
            let e = c.editor().clone();
            e.update(cx, |e, cx| e.replace_all("select 2".to_string(), w, cx));
            assert!(c.is_dirty(cx));
            c.save_file(w, cx);
            assert!(!c.is_dirty(cx));
        })
    });
    assert_eq!(std::fs::read_to_string(&sql).unwrap(), "select 2");
    // A console without a file is saved under .ferrodb/consoles.
    let plain = h.console(cx, "shop-dev", "select 3");
    h.with(cx, |w, cx| plain.update(cx, |c, cx| c.save_file(w, cx)));
    let saved = cx
        .update(|cx| plain.read(cx).file.clone())
        .expect("bound after save");
    assert!(saved.starts_with(h.dir.path().join(".ferrodb").join("consoles")));
    assert_eq!(std::fs::read_to_string(saved).unwrap(), "select 3");
}

#[gpui_kit::test]
async fn local_history_lists_revisions_and_restores(cx: &mut TestAppContext) {
    let h = start(cx);
    // A unique document: the mock's history is shared by tests in this process.
    let dir = h.dir.path().join("lh");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("q.sql");
    std::fs::write(&file, "select 'v1'").unwrap();
    let f2 = file.clone();
    h.with(cx, |w, cx| {
        ferrodb_ui::workspace::open_file_console(None, f2, w, cx).unwrap()
    });
    let console = cx.update(|cx| {
        h.ws.read(cx)
            .consoles()
            .into_iter()
            .find(|c| c.read(cx).file.as_deref() == Some(file.as_path()))
            .unwrap()
    });
    h.with(cx, |w, cx| console.update(cx, |c, cx| c.save_file(w, cx)));
    h.with(cx, |w, cx| {
        console.update(cx, |c, cx| {
            let e = c.editor().clone();
            e.update(cx, |e, cx| e.replace_all("select 'v2'".to_string(), w, cx));
            c.save_file(w, cx);
        })
    });
    let doc = cx.update(|cx| console.read(cx).history_document());
    let panel: Entity<LocalHistoryPanel> =
        h.with(cx, |w, cx| cx.new(|cx| LocalHistoryPanel::new(w, cx)));
    h.with(cx, |_, cx| {
        panel.update(cx, |p, cx| p.select_document(doc.clone(), cx))
    });
    let (revs, oldest) = cx.update(|cx| {
        let p = panel.read(cx);
        (p.revisions.len(), p.revisions.last().map(|r| r.id))
    });
    assert_eq!(revs, 2, "two saves, two revisions");
    // Restoring the first revision puts it back into the open console and
    // keeps the text it replaced.
    h.with(cx, |w, cx| {
        panel.update(cx, |p, cx| p.restore(oldest.unwrap(), w, cx))
    });
    cx.update(|cx| {
        assert_eq!(
            console.read(cx).editor().read(cx).value().to_string(),
            "select 'v1'"
        );
    });
    h.with(cx, |_, cx| panel.update(cx, |p, cx| p.reload(cx)));
    cx.update(|cx| {
        assert_eq!(
            panel.read(cx).revisions.len(),
            2,
            "same text as the newest: no new revision"
        )
    });
    let _ = actions::ShowLocalHistory;
}
