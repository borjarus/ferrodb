//! Main window content: title bar, toolbar, dock area and status bar.
//! Owns the project and persists its dock layout.

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use ferrodb_api::{ConnectionState, DataSourceId, NodeRef, QueryLanguage, SearchTarget};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _, TitleBar, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        dock::{
            DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, PanelStyle, panel_handle,
        },
        h_flex,
        menu::AppMenuBar,
        notification::Notification,
        status_bar::StatusBar,
        v_flex,
    },
    theme::{Appearance, is_dark},
};

use crate::{
    AppState,
    actions::{self, WORKSPACE_CONTEXT},
    i18n::{t, t_args},
    output,
    palette::{self, PaletteKind, PaletteModel, Target},
    panels::{
        ConsolePanel, ExplorerPanel, InspectorPanel, OutputPanel, ServicesPanel, WelcomePanel,
    },
    project::{LAYOUT_VERSION, Project},
    settings::SettingsStore,
    settings_view,
};

/// Delay before writing the layout after the last change.
const LAYOUT_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// Two Shift presses within this window open "Search Everywhere".
const DOUBLE_SHIFT: Duration = Duration::from_millis(400);

struct ActiveWorkspace(WeakEntity<Workspace>);
impl Global for ActiveWorkspace {}

#[derive(Default)]
struct ShiftTracker {
    down: bool,
    last_press: Option<Instant>,
}

impl ShiftTracker {
    /// Feeds a modifiers change; returns true on a double Shift.
    fn update(&mut self, m: &Modifiers, now: Instant) -> bool {
        let only_shift = m.shift && !m.control && !m.alt && !m.platform && !m.function;
        let pressed = only_shift && !self.down;
        self.down = m.shift;
        if !pressed {
            if m.control || m.alt || m.platform {
                self.last_press = None;
            }
            return false;
        }
        let double = self
            .last_press
            .is_some_and(|t| now.duration_since(t) <= DOUBLE_SHIFT);
        self.last_press = if double { None } else { Some(now) };
        double
    }
}

pub struct Workspace {
    focus: FocusHandle,
    dock: Entity<DockArea>,
    project: Project,
    menu_bar: Option<Entity<AppMenuBar>>,
    save_task: Option<Task<()>>,
    shift: ShiftTracker,
    /// Model of the last opened palette (inspected by UI tests).
    pub last_palette: Option<Rc<RefCell<PaletteModel>>>,
    _subscriptions: Vec<Subscription>,
    _events: Task<()>,
}

impl Workspace {
    pub fn new(project: Project, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (dock, skin) = DockSkin::dock_area("workspace", Some(LAYOUT_VERSION), window, cx);
        skin.set_panel_style(PanelStyle::TabBar, cx);

        let restored = match project.load_layout() {
            Some(state) => match dock.update(cx, |a, cx| a.load(state, window, cx)) {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "could not restore layout");
                    false
                }
            },
            None => false,
        };
        if !restored {
            Self::default_layout(&dock, window, cx);
        }

        let subscriptions = vec![
            cx.subscribe_in(&dock, window, |this, _, ev: &DockEvent, _, cx| {
                if matches!(ev, DockEvent::LayoutChanged) {
                    this.schedule_save(cx);
                }
            }),
            // Language / developer mode may change menu texts and structure.
            cx.observe_global::<SettingsStore>(|this: &mut Self, cx| {
                crate::menus::install(cx);
                if let Some(bar) = &this.menu_bar {
                    bar.update(cx, |b, cx| b.reload(cx));
                }
                cx.notify();
            }),
        ];

        // Status bar reflects connection changes.
        let rx = AppState::backend(cx).data_sources().subscribe();
        let events = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        let menu_bar = (!cfg!(target_os = "macos")).then(|| AppMenuBar::new(cx));
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        cx.set_global(ActiveWorkspace(cx.entity().downgrade()));

        output::info(
            t_args("project-opened", &[("name", project.name().into())]),
            cx,
        );
        let mut this = Self {
            focus,
            dock,
            project,
            menu_bar,
            save_task: None,
            shift: ShiftTracker::default(),
            last_palette: None,
            _subscriptions: subscriptions,
            _events: events,
        };
        // The initial layout was built before we subscribed: persist it once.
        this.schedule_save(cx);
        this
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn dock(&self) -> &Entity<DockArea> {
        &self.dock
    }

    /// Explorer left, consoles in the center, Services/Output at the bottom,
    /// Inspector on the right (closed).
    fn default_layout(dock: &Entity<DockArea>, window: &mut Window, cx: &mut App) {
        let first_source = AppState::backend(cx)
            .data_sources()
            .list()
            .first()
            .map(|s| s.config.id.clone());
        let explorer = cx.new(|cx| ExplorerPanel::new(window, cx));
        let welcome = cx.new(|cx| WelcomePanel::new(window, cx));
        let console = cx.new(|cx| ConsolePanel::new(first_source, String::new(), window, cx));
        let services = cx.new(|cx| ServicesPanel::new(window, cx));
        let output = cx.new(|cx| OutputPanel::new(window, cx));
        let inspector = cx.new(|cx| InspectorPanel::new(window, cx));
        dock.update(cx, |a, cx| {
            a.set_center(
                DockLayout::tabs()
                    .panel_view(panel_handle(welcome), cx)
                    .panel_view(panel_handle(console), cx),
                window,
                cx,
            );
            a.set_dock(
                DockPlacement::Left,
                DockLayout::tabs().panel_view(panel_handle(explorer), cx),
                window,
                cx,
            );
            a.set_dock_size(DockPlacement::Left, px(280.), window, cx);
            a.set_dock(
                DockPlacement::Bottom,
                DockLayout::tabs()
                    .panel_view(panel_handle(services), cx)
                    .panel_view(panel_handle(output), cx),
                window,
                cx,
            );
            a.set_dock_size(DockPlacement::Bottom, px(200.), window, cx);
            a.set_dock(
                DockPlacement::Right,
                DockLayout::tabs().panel_view(panel_handle(inspector), cx),
                window,
                cx,
            );
            a.set_dock_size(DockPlacement::Right, px(280.), window, cx);
            if a.is_dock_open(DockPlacement::Right) {
                a.toggle_dock(DockPlacement::Right, window, cx);
            }
        });
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LAYOUT_SAVE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.save_layout_now(cx));
        }));
    }

    /// Writes the layout immediately (also called on quit).
    pub fn save_layout_now(&mut self, cx: &mut App) {
        self.save_task = None;
        if AppState::global(cx).paths.is_none() {
            return; // tests / in-memory mode
        }
        let state = self.dock.read(cx).dump(cx);
        if let Err(e) = self.project.save_layout(state) {
            output::log(output::Level::Error, format!("layout: {e}"), cx);
        }
    }

    pub fn add_console(
        &mut self,
        data_source: Option<DataSourceId>,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = cx.new(|cx| ConsolePanel::new(data_source, text, window, cx));
        let editor = panel.read(cx).editor().clone();
        self.dock.update(cx, |a, cx| {
            a.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx)
        });
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    fn default_source(cx: &App) -> Option<DataSourceId> {
        let sources = AppState::backend(cx).data_sources().list();
        sources
            .iter()
            .find(|s| s.state == ConnectionState::Connected)
            .or(sources.first())
            .map(|s| s.config.id.clone())
    }

    /// Runs a palette target in this workspace.
    pub fn run_target(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            Target::Action(spec) => {
                window.focus(&self.focus, cx);
                window.dispatch_action((spec.build)(), cx);
            }
            Target::Search(entry) => match entry.target {
                SearchTarget::DataSource(id) => {
                    self.add_console(Some(id), String::new(), window, cx)
                }
                SearchTarget::Object(node) => {
                    let text = template_query(&node, cx);
                    self.add_console(Some(node.data_source.clone()), text, window, cx)
                }
                SearchTarget::File(_) => {}
            },
        }
    }

    fn open_palette(&mut self, kind: PaletteKind, window: &mut Window, cx: &mut Context<Self>) {
        if window.has_active_dialog(cx) {
            return;
        }
        let weak = cx.entity().downgrade();
        self.last_palette = Some(palette::open(kind, weak, window, cx));
    }

    fn toggle_dock(
        &mut self,
        placement: DockPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock
            .update(cx, |a, cx| a.toggle_dock(placement, window, cx));
    }

    fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dock = self.dock.clone();
        let project = self.project.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let dock = dock.clone();
            let project = project.clone();
            alert
                .title(t("reset-layout-title"))
                .description(t("reset-layout-description"))
                .confirm()
                .on_ok(move |_, window, cx| {
                    project.delete_layout();
                    Self::default_layout(&dock, window, cx);
                    window.push_notification(Notification::success(t("reset-layout-done")), cx);
                    true
                })
        });
    }

    fn open_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t("open-project-prompt").into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| this.switch_project(path, window, cx));
        })
        .detach();
    }

    /// Replaces the project of this window: saves the current layout, then
    /// rebuilds the dock from the new project's layout.
    pub fn switch_project(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.save_layout_now(cx);
        let project = match Project::open(&root) {
            Ok(p) => p,
            Err(e) => {
                window
                    .push_notification(Notification::error(format!("{}: {e}", root.display())), cx);
                return;
            }
        };
        SettingsStore::update(cx, |s| s.push_recent(&project.root));
        let restored = project.load_layout().is_some_and(|state| {
            self.dock
                .update(cx, |a, cx| a.load(state, window, cx))
                .is_ok()
        });
        if !restored {
            Self::default_layout(&self.dock, window, cx);
        }
        output::info(
            t_args("project-opened", &[("name", project.name().into())]),
            cx,
        );
        self.project = project;
        window.set_window_title(&format!("{} — {}", self.project.name(), t("app-name")));
        cx.notify();
    }

    fn recent_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let recent = SettingsStore::get(cx).recent_projects.clone();
        let weak = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let list = v_flex()
                .gap_1()
                .children(recent.iter().enumerate().map(|(ix, p)| {
                    let path = p.clone();
                    let weak = weak.clone();
                    Button::new(("recent", ix))
                        .ghost()
                        .w_full()
                        .label(p.display().to_string())
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let _ = weak
                                .update(cx, |ws, cx| ws.switch_project(path.clone(), window, cx));
                        })
                }));
            dialog
                .title(t("action-recent-projects"))
                .w(px(560.))
                .child(if recent.is_empty() {
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(t("recent-projects-empty"))
                        .into_any_element()
                } else {
                    list.into_any_element()
                })
        });
    }

    fn about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx).name().to_owned();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(t("action-about"))
                .w(px(420.))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_xl().child(t("app-name")))
                        .child(t_args(
                            "about-version",
                            &[("version", env!("CARGO_PKG_VERSION").into())],
                        ))
                        .child(div().text_color(cx.theme().muted_foreground).child(t_args(
                            "about-backend",
                            &[("backend", backend.clone().into())],
                        ))),
                )
                .footer(DialogFooter::new())
        });
    }

    fn scenarios(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(control) = AppState::global(cx).scenarios.clone() else {
            window.push_notification(Notification::info(t("dev-no-scenarios")), cx);
            return;
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let current = control.current();
            let buttons = control.scenarios().into_iter().map(|id| {
                let control = control.clone();
                Button::new(SharedString::from(format!("scenario-{id}")))
                    .w_full()
                    .when(id == current, |b| b.primary())
                    .when(id != current, |b| b.ghost())
                    .label(t(&format!("scenario-{id}")))
                    .on_click(move |_, window, cx| {
                        control.set(id);
                        crate::settings::SettingsStore::update(cx, |s| {
                            s.dev_scenario = id.to_owned()
                        });
                        window.close_dialog(cx);
                        let msg = t_args(
                            "dev-scenario-set",
                            &[("name", t(&format!("scenario-{id}")).into())],
                        );
                        output::info(msg.clone(), cx);
                        window.push_notification(Notification::info(msg), cx);
                        cx.refresh_windows();
                    })
            });
            dialog
                .title(t("action-dev-scenarios"))
                .w(px(420.))
                .child(v_flex().gap_1().children(buttons))
        });
    }

    fn on_modifiers(
        &mut self,
        ev: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.shift.update(&ev.modifiers, Instant::now()) {
            self.open_palette(PaletteKind::Everywhere, window, cx);
        }
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .w_full()
                .pr_2()
                .gap_2()
                .child(
                    icons::Icon::from(icons::Lucide::Database)
                        .small()
                        .text_color(cx.theme().primary),
                )
                .children(self.menu_bar.clone())
                .child(
                    Button::new("title-project")
                        .ghost()
                        .xsmall()
                        .label(self.project.name())
                        .tooltip(self.project.root.display().to_string())
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(actions::RecentProjects), cx)
                        }),
                )
                .child(div().flex_1())
                .child(
                    Button::new("title-search")
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Search)
                        .tooltip(t("action-search-everywhere"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(actions::SearchEverywhere), cx)
                        }),
                )
                .child(
                    Button::new("title-settings")
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Settings)
                        .tooltip(t("action-open-settings"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(actions::OpenSettings), cx)
                        }),
                ),
        )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tool = |id: &'static str, icon: icons::Lucide, tip: &str, action: Box<dyn Action>| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(t(tip))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        h_flex()
            .id("toolbar")
            .test_support()
            .gap_1()
            .px_2()
            .py_0p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().title_bar)
            .child(tool(
                "tb-new-ds",
                icons::Lucide::DatabasePlus,
                "action-new-data-source",
                Box::new(actions::NewDataSource),
            ))
            .child(tool(
                "tb-new-console",
                icons::Lucide::SquareTerminal,
                "action-new-console",
                Box::new(actions::NewConsole),
            ))
            .child(tool(
                "tb-execute",
                icons::Lucide::Play,
                "action-execute",
                Box::new(actions::ExecuteStatement),
            ))
            .child(tool(
                "tb-stop",
                icons::Lucide::Square,
                "action-stop",
                Box::new(actions::StopQuery),
            ))
            .child(div().flex_1())
            .child(tool(
                "tb-explorer",
                icons::Lucide::PanelLeft,
                "action-toggle-explorer",
                Box::new(actions::ToggleExplorer),
            ))
            .child(tool(
                "tb-bottom",
                icons::Lucide::PanelBottom,
                "action-toggle-bottom-panel",
                Box::new(actions::ToggleBottomPanel),
            ))
            .child(tool(
                "tb-right",
                icons::Lucide::PanelRight,
                "action-toggle-right-panel",
                Box::new(actions::ToggleRightPanel),
            ))
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = AppState::global(cx);
        let backend = state.backend.clone();
        let scenario = state.scenarios.as_ref().map(|s| s.current());
        let sources = backend.data_sources().list();
        let connected = sources
            .iter()
            .filter(|s| s.state == ConnectionState::Connected)
            .count();
        let muted = cx.theme().muted_foreground;
        StatusBar::new()
            .left(div().id("status-backend").test_support().child(t_args(
                "status-backend",
                &[("backend", backend.name().to_owned().into())],
            )))
            .left(div().id("status-connections").test_support().child(t_args(
                "status-connections",
                &[
                    ("connected", connected.into()),
                    ("total", sources.len().into()),
                ],
            )))
            .when_some(scenario.filter(|s| *s != "normal"), |bar, s| {
                bar.left(
                    div()
                        .text_color(cx.theme().warning)
                        .child(t(&format!("scenario-{s}"))),
                )
            })
            .right(
                div()
                    .text_color(muted)
                    .child(crate::i18n::locale().id().to_uppercase()),
            )
            .right(div().text_color(muted).child(if is_dark(cx) {
                t("appearance-dark")
            } else {
                t("appearance-light")
            }))
    }
}

/// The workspace of the most recently opened main window.
pub fn active(cx: &App) -> Option<Entity<Workspace>> {
    cx.try_global::<ActiveWorkspace>()
        .and_then(|w| w.0.upgrade())
}

/// Opens a console in the active workspace (used by panels).
pub fn open_console(
    data_source: Option<DataSourceId>,
    text: String,
    window: &mut Window,
    cx: &mut App,
) {
    if let Some(ws) = cx
        .try_global::<ActiveWorkspace>()
        .and_then(|w| w.0.upgrade())
    {
        ws.update(cx, |ws, cx| ws.add_console(data_source, text, window, cx));
    }
}

/// A starter query for an object, in the language of its data source.
fn template_query(node: &NodeRef, cx: &App) -> String {
    let name = node.name().unwrap_or_default();
    let lang = AppState::backend(cx)
        .data_sources()
        .get(&node.data_source)
        .map(|s| s.capabilities.language);
    match lang {
        Some(QueryLanguage::Mql) => format!("db.{name}.find({{}})"),
        Some(QueryLanguage::RedisCommands) => format!("SCAN 0 MATCH {name}*"),
        Some(QueryLanguage::PartiQl) => format!("SELECT * FROM \"{name}\""),
        Some(QueryLanguage::Cql | QueryLanguage::SqlPlusPlus) => {
            format!("SELECT * FROM {name} LIMIT 100;")
        }
        _ => format!("SELECT * FROM {name};"),
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        v_flex()
            .id("workspace")
            .test_support()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .text_size(cx.theme().font_size)
            .on_modifiers_changed(cx.listener(Self::on_modifiers))
            .on_action(cx.listener(|this, _: &actions::FindAction, w, cx| {
                this.open_palette(PaletteKind::Actions, w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::SearchEverywhere, w, cx| {
                this.open_palette(PaletteKind::Everywhere, w, cx)
            }))
            .on_action(
                cx.listener(|_, _: &actions::OpenSettings, w, cx| settings_view::open(w, cx)),
            )
            .on_action(cx.listener(|_, _: &actions::ToggleTheme, _, cx| {
                let next = if is_dark(cx) {
                    Appearance::Light
                } else {
                    Appearance::Dark
                };
                settings_view::change(cx, |s| s.appearance = next.id().into());
            }))
            .on_action(
                cx.listener(|this, _: &actions::OpenProject, w, cx| this.open_project(w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &actions::RecentProjects, w, cx| this.recent_projects(w, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::About, w, cx| this.about(w, cx)))
            .on_action(cx.listener(|this, _: &actions::ToggleExplorer, w, cx| {
                this.toggle_dock(DockPlacement::Left, w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ToggleBottomPanel, w, cx| {
                this.toggle_dock(DockPlacement::Bottom, w, cx)
            }))
            .on_action(cx.listener(|this, _: &actions::ToggleRightPanel, w, cx| {
                this.toggle_dock(DockPlacement::Right, w, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::ResetLayout, w, cx| this.reset_layout(w, cx)),
            )
            .on_action(cx.listener(|this, _: &actions::NewConsole, w, cx| {
                let ds = Self::default_source(cx);
                this.add_console(ds, String::new(), w, cx)
            }))
            .on_action(cx.listener(|_, _: &actions::NewDataSource, w, cx| {
                w.push_notification(
                    Notification::info(t_args("not-implemented-phase", &[("phase", 2.into())])),
                    cx,
                )
            }))
            .on_action(
                cx.listener(|this, _: &actions::DeveloperScenarios, w, cx| this.scenarios(w, cx)),
            )
            .on_action(cx.listener(|_, _: &actions::ShowNotificationDemo, w, cx| {
                w.push_notification(Notification::success(t("dev-notification-demo")), cx)
            }))
            // Console actions when no console is focused.
            .on_action(cx.listener(|_, _: &actions::ExecuteStatement, w, cx| {
                w.push_notification(Notification::info(t("console-focus-required")), cx)
            }))
            .on_action(cx.listener(|_, _: &actions::StopQuery, _, _| {}))
            .child(self.render_title_bar(cx))
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.dock.clone()))
            .child(self.render_status_bar(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(shift: bool, control: bool) -> Modifiers {
        Modifiers {
            shift,
            control,
            ..Default::default()
        }
    }

    #[::core::prelude::v1::test]
    fn double_shift_detection() {
        let mut s = ShiftTracker::default();
        let t0 = Instant::now();
        assert!(!s.update(&mods(true, false), t0));
        assert!(!s.update(&mods(false, false), t0 + Duration::from_millis(50)));
        assert!(s.update(&mods(true, false), t0 + Duration::from_millis(150)));
        // too slow
        s.update(&mods(false, false), t0 + Duration::from_millis(200));
        s.update(&mods(true, false), t0 + Duration::from_millis(300));
        s.update(&mods(false, false), t0 + Duration::from_millis(350));
        assert!(!s.update(&mods(true, false), t0 + Duration::from_millis(1000)));
        // ctrl+shift does not count
        s.update(&mods(false, false), t0 + Duration::from_millis(1050));
        assert!(!s.update(&mods(true, true), t0 + Duration::from_millis(1100)));
    }
}
