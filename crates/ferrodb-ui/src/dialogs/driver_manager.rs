//! Driver manager (plan 2.2): status of every driver (built-in / to
//! download / downloaded / update available / custom), download with
//! progress, version selection, licenses, custom artifacts, and the state of
//! the JDBC bridge process.

use std::collections::HashMap;

use ferrodb_api::{
    AccessLayer, BridgeState, DriverDescriptor, DriverEvent, DriverId, DriverStatus, Requirement,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        menu::{DropdownMenu as _, PopupMenuItem},
        progress::Progress,
        v_flex,
    },
};

use super::data_source::{download, status_text};
use crate::{
    AppState,
    i18n::{t, t_args},
    output,
};

pub struct DriverManager {
    drivers: Vec<DriverDescriptor>,
    pub statuses: HashMap<DriverId, DriverStatus>,
    versions: HashMap<DriverId, String>,
    pub bridge: BridgeState,
    _events: Task<()>,
}

impl DriverManager {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let backend = AppState::backend(cx);
        let catalog = backend.drivers();
        let drivers = catalog.list();
        let statuses = drivers
            .iter()
            .map(|d| (d.id.clone(), catalog.status(&d.id)))
            .collect();
        let versions = drivers
            .iter()
            .filter_map(|d| {
                catalog
                    .versions(&d.id)
                    .first()
                    .map(|v| (d.id.clone(), v.clone()))
            })
            .collect();
        let rx = catalog.subscribe();
        let events = cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                let ok = this.update(cx, |this, cx| {
                    match ev {
                        DriverEvent::StatusChanged(id, st) => {
                            this.statuses.insert(id, st);
                        }
                        DriverEvent::BridgeChanged(b) => this.bridge = b,
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        });
        Self {
            drivers,
            statuses,
            versions,
            bridge: catalog.jdbc_bridge(),
            _events: events,
        }
    }

    fn choose_files(&mut self, id: DriverId, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(t("drivers-choose-files").into()),
        });
        let backend = AppState::backend(cx);
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let r = backend.drivers().use_custom_artifacts(&id, paths).await;
            cx.update(|cx| {
                if let Err(e) = r {
                    output::log(output::Level::Error, e.to_string(), cx)
                }
            });
        })
        .detach();
    }

    fn render_row(&self, d: &DriverDescriptor, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let st = self
            .statuses
            .get(&d.id)
            .cloned()
            .unwrap_or(DriverStatus::NotDownloaded);
        let licenses: Vec<String> = d
            .requirements
            .iter()
            .map(|r| match r {
                Requirement::JdbcArtifact { license, .. }
                | Requirement::AdbcLibrary { license, .. } => license.clone(),
                Requirement::JavaRuntime { min_version } => format!("Java {min_version}+"),
            })
            .collect();
        let native = d.access() == AccessLayer::Native;
        let versions = AppState::backend(cx).drivers().versions(&d.id);
        let chosen = self.versions.get(&d.id).cloned();
        let downloading = matches!(st, DriverStatus::Downloading { .. });
        let (id1, id2, id3) = (d.id.clone(), d.id.clone(), d.id.clone());
        let weak = cx.entity().downgrade();
        h_flex()
            .id(SharedString::from(format!("driver-{}", d.id)))
            .test_support()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                icons::data_source_icon(d.capabilities.data_model)
                    .text_color(theme.muted_foreground),
            )
            .child(
                v_flex().w(px(200.)).child(d.name.clone()).child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} · {:?}", d.family, d.access())),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .gap_0p5()
                    .child(div().text_sm().child(status_text(&st)))
                    .when_some(
                        match st {
                            DriverStatus::Downloading { progress_permille } => {
                                Some(progress_permille)
                            }
                            _ => None,
                        },
                        |c, p| c.child(Progress::new("driver-progress").value(p as f32 / 10.0)),
                    )
                    .when(!licenses.is_empty(), |c| {
                        c.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(t_args(
                                    "drivers-license",
                                    &[("license", licenses.join(", ").into())],
                                )),
                        )
                    }),
            )
            .when(!native, |row| {
                row.child(
                    Button::new(SharedString::from(format!("driver-version-{}", d.id)))
                        .ghost()
                        .xsmall()
                        .label(chosen.clone().unwrap_or_default())
                        .dropdown_menu(move |menu, _, _| {
                            versions.iter().fold(menu, |m, v| {
                                let (weak, id, v2) = (weak.clone(), id3.clone(), v.clone());
                                m.item(PopupMenuItem::new(v.clone()).on_click(move |_, _, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.versions.insert(id.clone(), v2.clone());
                                        cx.notify();
                                    });
                                }))
                            })
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("driver-download-{}", d.id)))
                        .xsmall()
                        .primary()
                        .icon(icons::Lucide::Download)
                        .label(if st.is_ready() {
                            t("drivers-redownload")
                        } else {
                            t("drivers-download")
                        })
                        .loading(downloading)
                        .disabled(downloading)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let v = this.versions.get(&id1).cloned();
                            download(id1.clone(), v, cx)
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("driver-custom-{}", d.id)))
                        .xsmall()
                        .ghost()
                        .icon(icons::Lucide::FileUp)
                        .tooltip(t("drivers-custom-files"))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.choose_files(id2.clone(), cx)),
                        ),
                )
            })
    }
}

pub fn bridge_text(b: &BridgeState) -> String {
    match b {
        BridgeState::NotStarted => t("bridge-not-started"),
        BridgeState::Starting => t("bridge-starting"),
        BridgeState::Running { pid, connections } => t_args(
            "bridge-running",
            &[("pid", (*pid).into()), ("count", (*connections).into())],
        ),
        BridgeState::Restarting => t("bridge-restarting"),
        BridgeState::Failed(e) => format!("{}: {e}", t("state-failed")),
    }
}

impl Render for DriverManager {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let drivers = self.drivers.clone();
        let rows: Vec<AnyElement> = drivers
            .iter()
            .map(|d| self.render_row(d, cx).into_any_element())
            .collect();
        v_flex()
            .id("driver-manager")
            .test_support()
            .h(px(520.))
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .p_1()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .text_sm()
                    .child(icons::Icon::from(icons::Lucide::Cpu).small())
                    .child(t("bridge-title"))
                    .child(
                        div()
                            .id("bridge-state")
                            .test_support()
                            .child(bridge_text(&self.bridge)),
                    ),
            )
            .child(
                div()
                    .id("driver-rows")
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }
}

/// Most recently opened driver manager (UI tests).
#[derive(Default)]
pub struct LastManager(pub Option<Entity<DriverManager>>);
impl Global for LastManager {}

pub fn open(window: &mut Window, cx: &mut App) {
    let manager = cx.new(DriverManager::new);
    cx.set_global(LastManager(Some(manager.clone())));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t("action-driver-manager"))
            .w(px(860.))
            .child(manager.clone())
    });
}
