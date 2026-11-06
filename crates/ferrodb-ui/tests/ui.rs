//! UI tests without a visible window (GPUI Kit `test-support` + MockBackend).
//! Phase 1 completion criteria: startup, layout persistence, every action
//! reachable from the palette.

use std::{path::Path, sync::Arc, time::Duration};

use ferrodb_mock::MockBackend;
use ferrodb_ui::{
    AppPaths, AppState, Workspace,
    actions::{self, ACTIONS},
    palette::Target,
    project::Project,
};
use gpui_kit::{
    App, AppContext as _, Bounds, Entity, Point, TestAppContext, WindowBounds, WindowHandle,
    WindowOptions,
    base::Root,
    component::dock::DockPlacement,
    px, size,
    test::{TestAppContextExt as _, TestWindowExt as _},
};

fn init(cx: &mut TestAppContext, paths: Option<AppPaths>) {
    cx.update(|cx: &mut App| {
        ferrodb_ui::init(
            AppState {
                backend: Arc::new(MockBackend::deterministic()),
                scenarios: None,
                paths,
            },
            cx,
        );
        cx.set_reduce_motion(true);
    });
}

fn open(cx: &mut TestAppContext, project: Project) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(|cx| {
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(1200.), px(800.)),
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
    })
}

fn paths(dir: &Path) -> AppPaths {
    AppPaths {
        config_dir: dir.join("config"),
        data_dir: dir.join("data"),
    }
}

#[gpui_kit::test]
async fn app_starts_with_default_layout(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    init(cx, None);
    let (handle, ws) = open(cx, Project::open(dir.path()).unwrap());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("workspace").visible());
        assert!(window.find("explorer").visible(), "explorer docked left");
        assert!(
            window.find("welcome").visible(),
            "welcome tab active in the center"
        );
        assert!(window.find("status-backend").visible());
        assert!(window.find("toolbar").visible());
    })
    .unwrap();
    cx.update(|cx| {
        let dock = ws.read(cx).dock().read(cx);
        assert!(dock.is_dock_open(DockPlacement::Left));
        assert!(dock.is_dock_open(DockPlacement::Bottom));
        assert!(
            !dock.is_dock_open(DockPlacement::Right),
            "inspector starts closed"
        );
    });
}

#[gpui_kit::test]
async fn layout_survives_restart(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    init(cx, Some(paths(dir.path())));
    let root = dir.path().join("project");

    let (handle, ws) = open(cx, Project::open(&root).unwrap());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(actions::ToggleExplorer), cx);
        window.dispatch_action(Box::new(actions::ToggleRightPanel), cx);
        window.dispatch_action(Box::new(actions::NewConsole), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let dock = ws.read(cx).dock().read(cx);
        assert!(!dock.is_dock_open(DockPlacement::Left));
        assert!(dock.is_dock_open(DockPlacement::Right));
        ws.update(cx, |ws, cx| ws.save_layout_now(cx));
    });
    assert!(root.join(".ferrodb/layout.json").is_file());
    cx.update_window(handle.into(), |_, window, _| window.remove_window())
        .unwrap();

    // "Restart": a new workspace on the same project.
    let (handle, ws) = open(cx, Project::open(&root).unwrap());
    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    cx.update(|cx| {
        let dock = ws.read(cx).dock().read(cx);
        assert!(
            !dock.is_dock_open(DockPlacement::Left),
            "closed explorer restored"
        );
        assert!(
            dock.is_dock_open(DockPlacement::Right),
            "opened inspector restored"
        );
        let saved = dock.dump(cx);
        let center = serde_json::to_string(&saved.center).unwrap();
        assert_eq!(
            center.matches("\"Console\"").count(),
            2,
            "both consoles restored: {center}"
        );
    });
}

#[gpui_kit::test]
async fn find_action_lists_every_action(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    init(cx, None);
    let (handle, ws) = open(cx, Project::open(dir.path()).unwrap());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(actions::FindAction), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("dialog").is_some()
    })
    .await;
    cx.update(|cx| {
        let model = ws.read(cx).last_palette.clone().expect("palette opened");
        let model = model.borrow();
        assert_eq!(model.entries.len(), ACTIONS.len());
        for spec in ACTIONS {
            assert!(
                model
                    .entries
                    .iter()
                    .any(|e| matches!(e.target, Target::Action(s) if std::ptr::eq(s, spec))),
                "{} missing from Find Action",
                spec.action_name()
            );
        }
    });
}

#[gpui_kit::test]
async fn search_everywhere_indexes_mock_objects(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    init(cx, None);
    let (handle, ws) = open(cx, Project::open(dir.path()).unwrap());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(actions::SearchEverywhere), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let model = ws.read(cx).last_palette.clone().expect("palette opened");
        let mut model = model.borrow_mut();
        model.set_query("custmrs");
        let best = model.matched(0).expect("fuzzy match");
        assert_eq!(best.label, "customers");
        assert!(matches!(best.target, Target::Search(_)));
    });
}

#[gpui_kit::test]
async fn palette_runs_actions_in_the_workspace(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    init(cx, None);
    let (handle, ws) = open(cx, Project::open(dir.path()).unwrap());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let spec = ACTIONS
            .iter()
            .find(|a| a.label_id == "action-toggle-bottom-panel")
            .unwrap();
        ws.update(cx, |ws, cx| ws.run_target(Target::Action(spec), window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            !ws.read(cx)
                .dock()
                .read(cx)
                .is_dock_open(DockPlacement::Bottom)
        )
    });
}
