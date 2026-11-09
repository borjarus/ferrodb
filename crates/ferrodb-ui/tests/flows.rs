//! Phase 2 completion criteria: key flows on mock data, without a visible
//! window (GPUI Kit `test-support` + deterministic `MockBackend`).
//!
//! Flows: explorer (lazy tree, filter, open data), data source dialog
//! (26 drivers, form, test, save), driver download → connect, console
//! (results, paging, errors, cost estimate, explain), grid editing with
//! operation preview, documents, keys, Services, history, load scenarios
//! and the component gallery.

use std::sync::Arc;

use ferrodb_api::{
    BridgeState, CancelHandle, ConnectionState, DataSourceId, DriverId, DriverStatus, ExecMode,
    KeyValue, NodeRef, ObjectKind, QueryId,
};
use ferrodb_mock::{MockBackend, Scenario};
use ferrodb_ui::{
    AppState, Workspace, actions,
    activity::{self, RunningQuery},
    dialogs::{
        data_source::{LastForm, TestState},
        driver_manager::LastManager,
    },
    panels::{
        ConsolePanel, ExplorerPanel, GalleryPanel, HistoryPanel, ServicesPanel, console::RunTarget,
        explorer::Row,
    },
    project::Project,
    results::{ResultView, changes::LastPreview, documents::DocMode},
    workspace::{self, ActiveExplorer},
};
use ferrodb_ui_kit::kit::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Entity, Point, TestAppContext, Window, WindowBounds,
    WindowHandle, WindowOptions, base::Root, px, size, test::TestWindowExt as _,
};

struct Harness {
    mock: MockBackend,
    window: WindowHandle<Root>,
    ws: Entity<Workspace>,
    _dir: tempfile::TempDir,
}

fn start(cx: &mut TestAppContext) -> Harness {
    let mock = MockBackend::deterministic();
    let backend = mock.clone();
    cx.update(|cx: &mut App| {
        ferrodb_ui::init(
            AppState {
                backend: Arc::new(backend),
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
    let h = Harness {
        mock,
        window,
        ws,
        _dir: dir,
    };
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

    fn explorer(&self, cx: &mut TestAppContext) -> Entity<ExplorerPanel> {
        cx.update(|cx| cx.global::<ActiveExplorer>().0.upgrade().expect("explorer"))
    }

    fn expand(&self, cx: &mut TestAppContext, id: &str) {
        let e = self.explorer(cx);
        let id = id.to_owned();
        self.with(cx, |_, cx| e.update(cx, |e, cx| e.expand(id.into(), cx)));
    }

    fn visible(&self, cx: &mut TestAppContext) -> Vec<String> {
        let e = self.explorer(cx);
        cx.update(|cx| {
            e.read(cx)
                .visible_ids(cx)
                .iter()
                .map(|s| s.to_string())
                .collect()
        })
    }

    /// Opens a console on `ds` with `text` and runs the whole script.
    fn run(&self, cx: &mut TestAppContext, ds: &str, text: &str) -> Entity<ConsolePanel> {
        let ws = self.ws.clone();
        let (ds, text) = (ds.to_owned(), text.to_owned());
        let console = self.with(cx, |w, cx| {
            ws.update(cx, |ws, cx| {
                ws.add_console(Some(DataSourceId::new(ds)), text, w, cx)
            })
        });
        self.with(cx, |w, cx| {
            console.update(cx, |c, cx| c.run(RunTarget::Script, w, cx))
        });
        self.frame(cx);
        console
    }

    fn result(&self, cx: &mut TestAppContext, console: &Entity<ConsolePanel>) -> ResultView {
        cx.update(|cx| {
            console
                .read(cx)
                .result_view(1)
                .cloned()
                .expect("a result tab")
        })
    }
}

fn node(ds: &str, path: &[(ObjectKind, &str)]) -> NodeRef {
    path.iter()
        .fold(NodeRef::root(DataSourceId::new(ds)), |n, (k, name)| {
            n.child(*k, *name)
        })
}

// ------------------------------------------------------------------ 2.1 explorer

#[gpui_kit::test]
async fn explorer_loads_tree_lazily_by_hierarchy(cx: &mut TestAppContext) {
    let h = start(cx);
    let before = h.visible(cx);
    assert!(before.iter().any(|i| i == "ds:shop-dev"));
    assert!(
        !before.iter().any(|i| i.starts_with("obj:shop-dev")),
        "nothing loaded yet"
    );

    h.expand(cx, "ds:shop-dev");
    h.expand(cx, "obj:shop-dev/Database:shop");
    h.expand(cx, "obj:shop-dev/Database:shop/Schema:public");
    let ids = h.visible(cx);
    assert!(
        ids.iter().any(|i| i.ends_with("Schema:public:Table")),
        "tables grouped under the schema: {ids:?}"
    );
    assert!(
        ids.iter()
            .any(|i| i == "obj:shop-dev/Database:shop/Schema:public/Table:customers")
    );
    // Expanding a source connects it.
    let state = cx.update(|cx| {
        AppState::backend(cx)
            .data_sources()
            .get(&DataSourceId::new("shop-dev"))
            .unwrap()
            .state
    });
    assert_eq!(state, ConnectionState::Connected);

    // Key-value sources: prefixes by separator.
    h.expand(cx, "ds:cache");
    h.expand(cx, "obj:cache/Database:db0");
    h.expand(cx, "obj:cache/Database:db0/KeyPrefix:app");
    let ids = h.visible(cx);
    assert!(
        ids.iter()
            .any(|i| i == "obj:cache/Database:db0/KeyPrefix:app/KeyPrefix:user"),
        "{ids:?}"
    );
}

#[gpui_kit::test]
async fn explorer_filter_keeps_matching_branches(cx: &mut TestAppContext) {
    let h = start(cx);
    h.expand(cx, "ds:shop-dev");
    h.expand(cx, "obj:shop-dev/Database:shop");
    h.expand(cx, "obj:shop-dev/Database:shop/Schema:public");
    let e = h.explorer(cx);
    h.with(cx, |w, cx| {
        e.update(cx, |e, cx| e.set_filter("invoices", w, cx))
    });
    let ids = h.visible(cx);
    assert!(ids.iter().any(|i| i.ends_with("Table:invoices")), "{ids:?}");
    assert!(!ids.iter().any(|i| i.ends_with("Table:customers")));
    assert!(
        !ids.iter().any(|i| i == "ds:catalog"),
        "unrelated sources hidden"
    );
    h.with(cx, |w, cx| e.update(cx, |e, cx| e.set_filter("", w, cx)));
    assert!(h.visible(cx).iter().any(|i| i == "ds:catalog"));
}

#[gpui_kit::test]
async fn opening_table_data_runs_a_query_into_the_grid(cx: &mut TestAppContext) {
    let h = start(cx);
    let table = node(
        "shop-dev",
        &[
            (ObjectKind::Database, "shop"),
            (ObjectKind::Schema, "public"),
            (ObjectKind::Table, "orders"),
        ],
    );
    h.with(cx, |w, cx| workspace::open_data(table, w, cx));
    h.frame(cx);
    let console = cx.update(|cx| h.ws.read(cx).consoles().last().cloned().unwrap());
    let text = cx.update(|cx| console.read(cx).editor().read(cx).value().to_string());
    assert_eq!(text, "SELECT * FROM public.orders;");
    let ResultView::Grid(grid) = h.result(cx, &console) else {
        panic!("tabular data opens in the grid")
    };
    cx.update(|cx| {
        let m = grid.read(cx).model().read(cx);
        assert_eq!(m.data.len(), 500, "first page");
        assert!(m.has_more);
    });
    h.with(cx, |w, _| assert!(w.find("result-grid").visible()));
}

// ------------------------------------------------------------------ 2.2 data sources and drivers

#[gpui_kit::test]
async fn data_source_dialog_lists_the_26_databases(cx: &mut TestAppContext) {
    let h = start(cx);
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::NewDataSource), cx)
    });
    h.frame(cx);
    let form = cx.update(|cx| cx.global::<LastForm>().0.clone().expect("form"));
    cx.update(|cx| assert_eq!(form.read(cx).drivers().len(), 26));
    h.with(cx, |w, cx| {
        form.update(cx, |f, cx| f.set_search("sql", w, cx))
    });
    cx.update(|cx| {
        let names: Vec<String> = form
            .read(cx)
            .matching_drivers(cx)
            .iter()
            .map(|d| d.name.clone())
            .collect();
        assert!(names.contains(&"PostgreSQL".to_owned()) && names.contains(&"MySQL".to_owned()));
        assert!(!names.contains(&"MongoDB".to_owned()), "{names:?}");
    });
    h.with(cx, |w, _| assert!(w.find("data-source-form").visible()));
}

#[gpui_kit::test]
async fn new_data_source_is_tested_saved_and_shown(cx: &mut TestAppContext) {
    let h = start(cx);
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::NewDataSource), cx)
    });
    let form = cx.update(|cx| cx.global::<LastForm>().0.clone().unwrap());
    h.with(cx, |w, cx| {
        form.update(cx, |f, cx| {
            f.select_driver(DriverId::new("mysql"), w, cx);
            f.set_name("billing", w, cx);
            f.set_property("database", "billing", w, cx);
        })
    });
    cx.update(|cx| {
        let c = form.read(cx).config(cx).unwrap();
        assert_eq!(c.driver.as_str(), "mysql");
        assert!(
            c.properties.iter().any(|(k, v)| k == "port" && v == "3306"),
            "defaults from the driver"
        );
        assert!(
            !c.properties.iter().any(|(k, _)| k == "password"),
            "secrets never in the config"
        );
        assert_eq!(c.secret_ref.as_deref(), Some("secret://billing"));
    });
    h.with(cx, |_, cx| form.update(cx, |f, cx| f.test_connection(cx)));
    cx.update(|cx| {
        assert!(
            matches!(form.read(cx).test, TestState::Ok(_)),
            "{:?}",
            form.read(cx).test
        )
    });
    h.with(cx, |w, cx| form.update(cx, |f, cx| f.save(w, cx)));
    h.frame(cx);
    assert!(h.visible(cx).iter().any(|i| i == "ds:billing"));
}

#[gpui_kit::test]
async fn driver_flow_download_then_connect(cx: &mut TestAppContext) {
    let h = start(cx);
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::NewDataSource), cx)
    });
    let form = cx.update(|cx| cx.global::<LastForm>().0.clone().unwrap());
    h.with(cx, |w, cx| {
        form.update(cx, |f, cx| f.select_driver(DriverId::new("oracle"), w, cx))
    });
    cx.update(|cx| assert_eq!(form.read(cx).status(), Some(&DriverStatus::NotDownloaded)));
    // Testing fails until the driver is there.
    h.with(cx, |_, cx| form.update(cx, |f, cx| f.test_connection(cx)));
    cx.update(|cx| assert!(matches!(form.read(cx).test, TestState::Failed(_))));
    h.with(cx, |_, cx| form.update(cx, |f, cx| f.download_driver(cx)));
    cx.update(|cx| {
        assert!(matches!(
            form.read(cx).status(),
            Some(DriverStatus::Downloaded { .. })
        ))
    });
    h.with(cx, |w, cx| {
        form.update(cx, |f, cx| {
            f.set_name("erp", w, cx);
            f.set_property("host", "db.local", w, cx);
            f.test_connection(cx)
        })
    });
    cx.update(|cx| assert!(matches!(form.read(cx).test, TestState::Ok(_))));
    h.with(cx, |w, cx| form.update(cx, |f, cx| f.save(w, cx)));
    // Connect (expanding the source) starts the JDBC bridge.
    h.expand(cx, "ds:erp");
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::OpenDriverManager), cx)
    });
    let manager = cx.update(|cx| cx.global::<LastManager>().0.clone().unwrap());
    cx.update(|cx| {
        let m = manager.read(cx);
        assert!(
            matches!(m.bridge, BridgeState::Running { connections: 1, .. }),
            "{:?}",
            m.bridge
        );
        assert!(matches!(
            m.statuses.get(&DriverId::new("oracle")),
            Some(DriverStatus::Downloaded { .. })
        ));
    });
    h.with(cx, |w, _| assert!(w.find("driver-manager").visible()));
}

// ------------------------------------------------------------------ 2.3 / 2.4 console and grid

#[gpui_kit::test]
async fn console_streams_results_and_pages(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "shop-dev", "-- customers\nselect * from customers");
    let ResultView::Grid(grid) = h.result(cx, &console) else {
        panic!()
    };
    cx.update(|cx| {
        let c = console.read(cx);
        assert!(!c.is_running());
        assert_eq!(c.tabs.len(), 1);
        let m = grid.read(cx).model().read(cx);
        assert_eq!(m.title, "customers", "named result tab");
        assert_eq!((m.data.len(), m.has_more), (500, true));
    });
    let model = cx.update(|cx| grid.read(cx).model().clone());
    h.with(cx, |_, cx| {
        ferrodb_ui::results::ResultModel::fetch_more(&model, false, cx)
    });
    cx.update(|cx| {
        let m = model.read(cx);
        assert_eq!((m.data.len(), m.has_more), (1000, false));
    });
    // Copy as JSON / INSERT works on the loaded rows.
    cx.update(|cx| {
        let g = grid.read(cx);
        let json = g.rows_text(&[0], ferrodb_ui::results::grid::CopyFormat::Json, cx);
        assert!(json.contains("\"id\": 1"), "{json}");
        let ins = g.rows_text(&[0], ferrodb_ui::results::grid::CopyFormat::Insert, cx);
        assert!(ins.starts_with("INSERT INTO customers (id, name"), "{ins}");
    });
    h.with(cx, |w, _| assert!(w.find("grid-footer").visible()));
}

#[gpui_kit::test]
async fn query_errors_point_at_the_position(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "shop-dev", "select 1;\nselect error from t");
    cx.update(|cx| {
        let c = console.read(cx);
        assert!(c.messages.iter().any(|m| m.text.contains("syntax error")));
        assert_eq!(c.active_tab, 0, "errors switch to the output tab");
        let diags = c
            .editor()
            .read(cx)
            .diagnostics()
            .map(|d| d.len())
            .unwrap_or(0);
        assert_eq!(diags, 1, "error marked in the editor");
    });
}

#[gpui_kit::test]
async fn statement_under_cursor_or_selection_is_executed(cx: &mut TestAppContext) {
    let h = start(cx);
    let ws = h.ws.clone();
    let console = h.with(cx, |w, cx| {
        ws.update(cx, |ws, cx| {
            ws.add_console(
                Some(DataSourceId::new("shop-dev")),
                "select 1;\nselect 2;\nselect 3;".into(),
                w,
                cx,
            )
        })
    });
    // Cursor at the start → first statement.
    cx.update(|cx| {
        let (text, base, mode) = console
            .read(cx)
            .script_for(&RunTarget::Statement, cx)
            .unwrap();
        assert_eq!(
            (text.as_str(), base, mode),
            ("select 1", 0, ExecMode::StatementUnderCursor)
        );
        let (text, _, mode) = console.read(cx).script_for(&RunTarget::Script, cx).unwrap();
        assert_eq!(mode, ExecMode::WholeScript);
        assert!(text.contains("select 3"));
    });
}

#[gpui_kit::test]
async fn cost_is_shown_before_running_on_bigquery(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "analytics", "select * from events");
    cx.update(|cx| {
        let c = console.read(cx);
        assert!(c.cost.is_some(), "cost estimated");
        assert!(c.tabs.is_empty(), "nothing executed before confirmation");
    });
    h.with(cx, |w, cx| {
        assert!(w.has_active_dialog(cx), "cost confirmation shown");
        w.click("cost-run", cx);
    });
    h.frame(cx);
    cx.update(|cx| assert_eq!(console.read(cx).tabs.len(), 1, "ran after confirmation"));
}

#[gpui_kit::test]
async fn explain_opens_a_plan_tab(cx: &mut TestAppContext) {
    let h = start(cx);
    let ws = h.ws.clone();
    let console = h.with(cx, |w, cx| {
        ws.update(cx, |ws, cx| {
            ws.add_console(
                Some(DataSourceId::new("shop-dev")),
                "select * from a join b on a.id = b.a_id".into(),
                w,
                cx,
            )
        })
    });
    h.with(cx, |w, cx| console.update(cx, |c, cx| c.explain(w, cx)));
    h.frame(cx);
    h.with(cx, |w, _| assert!(w.find("plan-view").visible()));
}

#[gpui_kit::test]
async fn grid_edits_go_through_the_operation_preview(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "shop-dev", "select * from customers");
    let ResultView::Grid(grid) = h.result(cx, &console) else {
        panic!()
    };
    h.with(cx, |w, cx| {
        grid.update(cx, |g, cx| {
            g.commit_value(0, 1, Some("Zed".into()), w, cx);
            // Invalid value for an integer column is rejected.
            g.commit_value(1, 0, Some("not a number".into()), w, cx);
            g.submit(w, cx);
        })
    });
    h.frame(cx);
    let preview = cx.update(|cx| cx.global::<LastPreview>().0.clone());
    assert_eq!(preview, ["UPDATE customers SET name = 'Zed' WHERE id = 1;"]);
    h.with(cx, |w, cx| w.click("changes-apply", cx));
    h.frame(cx);
    cx.update(|cx| {
        let pending = grid.read(cx).table().read(cx).delegate().pending_count();
        assert_eq!(pending, 0, "applied changes are cleared");
    });
}

#[gpui_kit::test]
async fn production_sources_are_read_only(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "shop-prod", "select * from customers");
    let ResultView::Grid(grid) = h.result(cx, &console) else {
        panic!()
    };
    h.with(cx, |w, cx| {
        grid.update(cx, |g, cx| g.start_edit(0, 1, w, cx))
    });
    cx.update(|cx| assert_eq!(grid.read(cx).table().read(cx).delegate().pending_count(), 0));
    // A dangerous statement on a production source asks for confirmation
    // first (plan 2.5)…
    let console = h.run(cx, "shop-prod", "delete from orders");
    cx.update(|cx| {
        assert!(
            console.read(cx).messages.is_empty(),
            "not run before confirming"
        )
    });
    h.with(cx, |w, cx| w.click("risk-run", cx));
    h.frame(cx);
    // …and is then refused by the backend.
    cx.update(|cx| {
        assert!(
            console
                .read(cx)
                .messages
                .iter()
                .any(|m| m.text.contains("read-only"))
        )
    });
}

// ------------------------------------------------------------------ 2.5 documents

#[gpui_kit::test]
async fn documents_tree_json_table_schema_and_edit(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "catalog", "db.customers.find({})");
    let ResultView::Documents(docs) = h.result(cx, &console) else {
        panic!("documents open in the document browser")
    };
    cx.update(|cx| {
        let d = docs.read(cx);
        assert_eq!(d.model().read(cx).data.len(), 200);
        assert!(d.tree().read(cx).entry(0).is_some());
    });
    h.with(cx, |w, cx| {
        docs.update(cx, |d, cx| d.set_mode(DocMode::Json, w, cx))
    });
    cx.update(|cx| {
        let json = docs.read(cx).json_text(cx);
        assert!(
            json.starts_with('[') && json.contains("\"address\""),
            "{}",
            &json[..80]
        );
    });
    h.with(cx, |w, cx| {
        docs.update(cx, |d, cx| d.set_mode(DocMode::Table, w, cx))
    });
    h.with(cx, |_, cx| docs.update(cx, |d, cx| d.infer_schema(cx)));
    h.frame(cx);
    cx.update(|cx| {
        let s = docs.read(cx).schema.clone().expect("schema inferred");
        assert!(s.fields.iter().any(|f| f.path == "address.city"));
    });
    h.with(cx, |w, _| assert!(w.find("doc-schema").visible()));
    h.with(cx, |w, cx| {
        docs.update(cx, |d, cx| {
            d.stage_field(0, "age", ferrodb_api::bson::Bson::Int32(42), cx);
            d.submit(w, cx)
        })
    });
    let preview = cx.update(|cx| cx.global::<LastPreview>().0.clone());
    assert!(
        preview[0].starts_with("db.customers.updateOne({ _id: ")
            && preview[0].contains("\"age\": 42"),
        "{preview:?}"
    );
}

// ------------------------------------------------------------------ 2.6 keys

#[gpui_kit::test]
async fn keys_are_grouped_and_values_shown_per_type(cx: &mut TestAppContext) {
    let h = start(cx);
    let console = h.run(cx, "cache", "SCAN 0 MATCH app:user:* COUNT 500");
    let ResultView::Keys(keys) = h.result(cx, &console) else {
        panic!("keys open in the key browser")
    };
    let first = cx.update(|cx| {
        let k = keys.read(cx);
        let m = k.model().read(cx);
        assert!(m.data.len() > 10);
        let first = k
            .tree()
            .read(cx)
            .entry(0)
            .expect("tree")
            .item()
            .id
            .to_string();
        assert_eq!(first, "p:app:", "grouped by separator");
        match &m.data {
            ferrodb_ui::results::ResultData::Keys { keys, .. } => keys[0].clone(),
            _ => unreachable!(),
        }
    });
    h.with(cx, |_, cx| keys.update(cx, |k, cx| k.select_key(first, cx)));
    h.frame(cx);
    cx.update(|cx| assert!(matches!(keys.read(cx).value, Some(KeyValue::Hash(_)))));
    h.with(cx, |w, _| assert!(w.find("key-members").visible()));
}

// ------------------------------------------------------------------ 2.7 bottom panels

#[gpui_kit::test]
async fn services_history_and_local_history_follow_queries(cx: &mut TestAppContext) {
    let h = start(cx);
    h.run(cx, "shop-dev", "select * from orders");
    let (sessions, history) = h.with(cx, |w, cx| {
        let s = cx.new(|cx| ServicesPanel::new(w, cx));
        let hp = cx.new(|cx| HistoryPanel::new(w, cx));
        (s, hp)
    });
    cx.update(|cx| {
        assert_eq!(
            sessions.read(cx).sessions.len(),
            1,
            "console session listed"
        );
        let e = &history.read(cx).entries;
        assert!(
            e.iter().any(|x| x.text.contains("select * from orders")),
            "{e:?}"
        );
        let snaps = activity::entity(cx).unwrap().read(cx).snapshots.len();
        assert_eq!(snaps, 1, "local history snapshot taken on execution");
    });
    // Running queries can be cancelled from Services.
    let cancel = CancelHandle::new();
    h.with(cx, |_, cx| {
        activity::started(
            RunningQuery {
                id: QueryId(999),
                data_source: DataSourceId::new("shop-dev"),
                text: "select pg_sleep(60)".into(),
                started: std::time::Instant::now(),
                cancel: cancel.clone(),
            },
            cx,
        )
    });
    h.with(cx, |w, cx| w.render_frame(cx));
    h.with(cx, |w, cx| {
        assert!(w.try_find("running-999").is_some() || !w.find("services").visible());
        activity::cancel(QueryId(999), cx);
    });
    assert!(cancel.is_cancelled());
}

#[gpui_kit::test]
async fn jdbc_process_failure_is_reported(cx: &mut TestAppContext) {
    let h = start(cx);
    h.mock.set_scenario(Scenario::JdbcProcessFailure);
    // Deterministic mock: the download completes without an executor.
    futures::FutureExt::now_or_never(ferrodb_api::DriverCatalogService::download(
        &h.mock,
        &DriverId::new("oracle"),
        None,
    ))
    .expect("completes immediately")
    .unwrap();
    let console = h.run(cx, "legacy", "select * from dual");
    cx.update(|cx| {
        assert!(
            console
                .read(cx)
                .messages
                .iter()
                .any(|m| m.text.contains("JDBC bridge"))
        )
    });
}

// ------------------------------------------------------------------ load scenarios

#[gpui_kit::test]
async fn million_rows_scenario_pages_without_loading_everything(cx: &mut TestAppContext) {
    let h = start(cx);
    h.mock.set_scenario(Scenario::MillionRows);
    let console = h.run(cx, "shop-dev", "select * from big");
    let ResultView::Grid(grid) = h.result(cx, &console) else {
        panic!()
    };
    cx.update(|cx| {
        let m = grid.read(cx).model().read(cx);
        assert_eq!((m.data.len(), m.has_more), (500, true));
        assert_eq!(grid.read(cx).table().read(cx).delegate().pending_count(), 0);
        match &m.data {
            ferrodb_ui::results::ResultData::Table(t) => assert_eq!(t.schema.fields().len(), 50),
            _ => unreachable!(),
        }
    });
}

#[gpui_kit::test]
async fn ten_thousand_tables_in_the_explorer(cx: &mut TestAppContext) {
    let h = start(cx);
    h.mock.set_scenario(Scenario::TenThousandTables);
    h.expand(cx, "ds:shop-dev");
    h.expand(cx, "obj:shop-dev/Database:shop");
    h.expand(cx, "obj:shop-dev/Database:shop/Schema:public");
    let e = h.explorer(cx);
    let group = cx.update(|cx| {
        e.read(cx)
            .row("grp:obj:shop-dev/Database:shop/Schema:public:Table")
    });
    assert!(matches!(group, Some(Row::Group { count: 10_000, .. })));
}

#[gpui_kit::test]
async fn hundred_thousand_documents_and_million_keys_page(cx: &mut TestAppContext) {
    let h = start(cx);
    h.mock.set_scenario(Scenario::HundredThousandDocuments);
    let console = h.run(cx, "catalog", "db.customers.find({})");
    let ResultView::Documents(docs) = h.result(cx, &console) else {
        panic!()
    };
    cx.update(|cx| {
        let m = docs.read(cx).model().read(cx);
        assert_eq!((m.data.len(), m.has_more), (500, true));
    });
    h.mock.set_scenario(Scenario::MillionKeys);
    let console = h.run(cx, "cache", "SCAN 0 MATCH * COUNT 500");
    let ResultView::Keys(keys) = h.result(cx, &console) else {
        panic!()
    };
    cx.update(|cx| assert!(keys.read(cx).model().read(cx).has_more));
}

#[gpui_kit::test]
async fn five_mb_document_is_browsable(cx: &mut TestAppContext) {
    let h = start(cx);
    h.mock.set_scenario(Scenario::FiveMbDocument);
    let console = h.run(cx, "catalog", "db.customers.find({})");
    let ResultView::Documents(docs) = h.result(cx, &console) else {
        panic!()
    };
    cx.update(|cx| {
        let d = docs.read(cx);
        let first = d.tree().read(cx).entry(0).unwrap().item().clone();
        let items = first
            .children
            .iter()
            .find(|c| c.id.ends_with("|items"))
            .expect("items array");
        assert_eq!(
            items.children.len(),
            1001,
            "large arrays are capped in the tree"
        );
    });
}

// ------------------------------------------------------------------ 2.8 / 2.9

#[gpui_kit::test]
async fn mockup_views_open(cx: &mut TestAppContext) {
    let h = start(cx);
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::ShowErDiagram), cx)
    });
    h.frame(cx);
    h.with(cx, |w, _| assert!(w.find("er-diagram").visible()));
    h.with(cx, |w, cx| w.close_dialog(cx));
    h.with(cx, |w, cx| {
        w.dispatch_action(Box::new(actions::NewTable), cx)
    });
    h.frame(cx);
    h.with(cx, |w, _| assert!(w.find("table-editor").visible()));
}

#[gpui_kit::test]
async fn gallery_builds_every_component(cx: &mut TestAppContext) {
    let h = start(cx);
    let gallery = h.with(cx, |w, cx| cx.new(|cx| GalleryPanel::new(w, cx)));
    h.with(cx, |w, cx| gallery.update(cx, |g, cx| g.build_all(w, cx)));
    cx.update(|cx| {
        assert_eq!(
            gallery.read(cx).built(),
            ferrodb_ui::panels::gallery::SECTIONS.len()
        )
    });
}
