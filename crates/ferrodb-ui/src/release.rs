//! Phase 10 in the UI: crash reports offered after a crash, update checks
//! and the update dialog, the "Updates and Privacy" settings page, and the
//! Help menu commands (Check for Updates, Report a Problem, Join Beta
//! Program, About).
//!
//! Everything goes through `Backend::updates()` / `Backend::crash_reports()`;
//! the state shared by the status bar, the dialogs and the settings page is
//! the [`ReleaseHub`] global.

use std::{collections::HashSet, path::PathBuf};

use ferrodb_api::{
    API_VERSION, CancelHandle, CrashConsent, CrashReport, UpdateChannel, UpdateEvent, UpdateInfo,
    UpdateSettings,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dialog::DialogFooter,
        h_flex,
        input::{Input, InputState},
        notification::Notification,
        progress::Progress,
        switch::Switch,
        v_flex,
    },
};

use crate::{
    AppState,
    ai_settings::{choice, field_row, outcome, section},
    i18n::{t, t_args},
    output,
};

/// Where "Report a Problem" leads.
pub const ISSUES_URL: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/issues/new");

/// State of the update package download.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DownloadState {
    #[default]
    Idle,
    Running {
        done: u64,
        total: u64,
    },
    Ready(PathBuf),
    Failed(String),
}

/// Release state of the application.
#[derive(Default)]
pub struct ReleaseHub {
    /// The newer release found by the last check.
    pub update: Option<UpdateInfo>,
    pub download: DownloadState,
    cancel: Option<CancelHandle>,
    pub checking: bool,
    /// Outcome of the last check (`Ok(None)` = up to date).
    pub last_check: Option<Result<Option<UpdateInfo>, String>>,
    pub installing: bool,
    /// The crash dialog of this run (UI tests).
    pub crash_dialog: Option<Entity<CrashDialog>>,
    /// Startup work (crash reports, automatic check) ran already.
    started: bool,
}
impl Global for ReleaseHub {}

pub fn init(cx: &mut App) {
    cx.set_global(ReleaseHub::default());
}

pub fn hub(cx: &App) -> Option<&ReleaseHub> {
    cx.try_global::<ReleaseHub>()
}

fn update_hub(cx: &mut App, f: impl FnOnce(&mut ReleaseHub)) {
    if cx.try_global::<ReleaseHub>().is_none() {
        init(cx);
    }
    cx.update_global::<ReleaseHub, _>(|h, _| f(h));
    cx.refresh_windows();
}

fn channel_label(c: UpdateChannel) -> String {
    match c {
        UpdateChannel::Stable => t("update-channel-stable"),
        UpdateChannel::Beta => t("update-channel-beta"),
    }
}

fn consent_label(c: CrashConsent) -> String {
    match c {
        CrashConsent::Ask => t("crash-consent-ask"),
        CrashConsent::Always => t("crash-consent-always"),
        CrashConsent::Never => t("crash-consent-never"),
    }
}

/// Version of the running application.
pub fn app_version(cx: &App) -> String {
    let v = AppState::backend(cx).updates().current_version();
    if v.is_empty() {
        env!("CARGO_PKG_VERSION").to_owned()
    } else {
        v
    }
}

// ------------------------------------------------------------------ startup

/// Startup work, once per process: offer (or send) crash reports from the
/// last run and run the automatic update check when it is due. Runs after
/// the window is built (dialogs need its root).
pub fn startup(window: &mut Window, cx: &mut App) -> Task<()> {
    if hub(cx).is_some_and(|h| h.started) {
        return Task::ready(());
    }
    update_hub(cx, |h| h.started = true);
    let backend = AppState::backend(cx);
    // Reading the reports directory is I/O: off the UI thread.
    let probe = cx.background_executor().spawn(async move {
        let crashes = backend.crash_reports();
        let pending = if crashes.available() {
            crashes.pending()
        } else {
            Vec::new()
        };
        let due = backend.updates().available() && backend.updates().check_due();
        (pending, crashes.consent(), due)
    });
    window.spawn(cx, async move |cx| {
        let (pending, consent, due) = probe.await;
        let _ = cx.update(|window, cx| {
            if !pending.is_empty() {
                match consent {
                    CrashConsent::Ask => open_crash_dialog(pending, window, cx),
                    CrashConsent::Always => {
                        send_silently(pending.into_iter().map(|r| r.id).collect(), cx)
                    }
                    CrashConsent::Never => {}
                }
            }
            if due {
                check(false, window, cx);
            }
        });
    })
}

fn send_silently(ids: Vec<String>, cx: &mut App) {
    let backend = AppState::backend(cx);
    let n = ids.len();
    let task = cx
        .background_executor()
        .spawn(async move { backend.crash_reports().send(ids, String::new()).await });
    cx.spawn(async move |cx| {
        let r = task.await;
        cx.update(|cx| match r {
            Ok(()) => output::info(t_args("crash-sent-auto", &[("count", n.into())]), cx),
            Err(e) => output::log(output::Level::Warning, format!("crash reports: {e}"), cx),
        });
    })
    .detach();
}

// ------------------------------------------------------------------ updates

/// Checks for an update. `manual`: from a command (shows the dialog or "up
/// to date"); otherwise the automatic check (a notification when found,
/// failures only in the output log).
pub fn check(manual: bool, window: &mut Window, cx: &mut App) {
    let backend = AppState::backend(cx);
    if !backend.updates().available() {
        if manual {
            window.push_notification(Notification::info(t("update-unavailable")), cx);
        }
        return;
    }
    if hub(cx).is_some_and(|h| h.checking) {
        return;
    }
    update_hub(cx, |h| h.checking = true);
    let task = cx
        .background_executor()
        .spawn(async move { backend.updates().check().await });
    window
        .spawn(cx, async move |cx| {
            let r = task.await.map_err(|e| e.to_string());
            let _ = cx.update(|window, cx| finish_check(r, manual, window, cx));
        })
        .detach();
}

fn finish_check(
    r: Result<Option<UpdateInfo>, String>,
    manual: bool,
    window: &mut Window,
    cx: &mut App,
) {
    update_hub(cx, |h| {
        h.checking = false;
        h.last_check = Some(r.clone());
        if let Ok(Some(u)) = &r {
            // A newer find replaces an older one (and its download).
            if h.update.as_ref().is_none_or(|old| old.version != u.version) {
                h.update = Some(u.clone());
                h.download = DownloadState::Idle;
            }
        }
    });
    match r {
        Ok(Some(_)) if manual => open_update_dialog(window, cx),
        Ok(Some(u)) => {
            output::info(
                t_args("update-available", &[("version", u.version.clone().into())]),
                cx,
            );
            window.push_notification(
                Notification::info(t_args("update-available", &[("version", u.version.into())]))
                    .action(|_, _, _| {
                        Button::new("update-notification-view")
                            .small()
                            .primary()
                            .label(t("update-view"))
                            .on_click(|_, window, cx| open_update_dialog(window, cx))
                    }),
                cx,
            );
        }
        Ok(None) => {
            if manual {
                window.push_notification(
                    Notification::success(t_args(
                        "update-none",
                        &[("version", app_version(cx).into())],
                    )),
                    cx,
                );
            }
        }
        Err(e) => {
            output::log(output::Level::Warning, format!("update check: {e}"), cx);
            if manual {
                window.push_notification(
                    Notification::error(t_args("update-check-failed", &[("error", e.into())])),
                    cx,
                );
            }
        }
    }
}

/// Opens the dialog of the found update (checks first when none is known).
pub fn open_update_dialog(window: &mut Window, cx: &mut App) {
    if hub(cx).and_then(|h| h.update.as_ref()).is_none() {
        check(true, window, cx);
        return;
    }
    window.open_dialog(cx, |dialog, _, cx| {
        let Some(h) = hub(cx) else {
            return dialog;
        };
        let Some(u) = h.update.clone() else {
            return dialog;
        };
        let theme = cx.theme().clone();
        let state = h.download.clone();
        let installing = h.installing;
        let current = app_version(cx);
        let progress =
            match &state {
                DownloadState::Idle => None,
                DownloadState::Running { done, total } => {
                    let pct = if *total > 0 {
                        (*done as f64 * 100. / *total as f64) as f32
                    } else {
                        0.
                    };
                    Some(
                        v_flex()
                            .gap_1()
                            .child(Progress::new("update-progress").value(pct))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                t_args(
                                    "update-progress",
                                    &[
                                        ("done", format_size(*done).into()),
                                        ("total", format_size(*total).into()),
                                    ],
                                ),
                            ))
                            .into_any_element(),
                    )
                }
                DownloadState::Ready(_) => Some(
                    div()
                        .id("update-ready")
                        .test_support()
                        .text_sm()
                        .text_color(theme.success)
                        .child(t("update-ready"))
                        .into_any_element(),
                ),
                DownloadState::Failed(e) => Some(
                    div()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(t_args("update-failed", &[("error", e.clone().into())]))
                        .into_any_element(),
                ),
            };
        let footer = match &state {
            DownloadState::Idle | DownloadState::Failed(_) => DialogFooter::new()
                .child(
                    Button::new("update-later")
                        .label(t("update-later"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("update-download")
                        .primary()
                        .label(t("update-download"))
                        .on_click(|_, window, cx| start_download(window, cx)),
                ),
            DownloadState::Running { .. } => DialogFooter::new().child(
                Button::new("update-cancel")
                    .label(t("update-cancel"))
                    .on_click(|_, _, cx| cancel_download(cx)),
            ),
            DownloadState::Ready(_) => DialogFooter::new()
                .child(
                    Button::new("update-later")
                        .label(t("update-later"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("update-install")
                        .primary()
                        .label(t("update-install"))
                        .loading(installing)
                        .on_click(|_, window, cx| install_and_restart(window, cx)),
                ),
        };
        dialog
            .title(t_args(
                "update-title",
                &[("version", u.version.clone().into())],
            ))
            .w(px(560.))
            .child(
                v_flex()
                    .id("update-dialog")
                    .test_support()
                    .gap_2()
                    .child(div().text_sm().child(t_args(
                        "update-description",
                        &[
                            ("version", u.version.clone().into()),
                            ("current", current.into()),
                            ("date", u.date.clone().into()),
                        ],
                    )))
                    .when(u.channel == UpdateChannel::Beta, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.warning)
                                .child(t("update-is-beta")),
                        )
                    })
                    .child(
                        div()
                            .id("update-notes")
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .text_sm()
                            .child(if u.notes.trim().is_empty() {
                                t("update-no-notes")
                            } else {
                                u.notes.clone()
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(t_args(
                                "update-size",
                                &[("size", format_size(u.size).into())],
                            )),
                    )
                    .children(progress),
            )
            .footer(footer)
    });
}

fn format_size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024. * 1024.);
    if mb >= 1. {
        format!("{mb:.1} MB")
    } else {
        format!("{:.0} KB", bytes as f64 / 1024.)
    }
}

/// Starts downloading the found update; events update the hub.
pub fn start_download(window: &mut Window, cx: &mut App) {
    let Some(u) = hub(cx).and_then(|h| h.update.clone()) else {
        return;
    };
    if matches!(
        hub(cx).map(|h| &h.download),
        Some(DownloadState::Running { .. })
    ) {
        return;
    }
    let d = AppState::backend(cx).updates().download(u.clone());
    update_hub(cx, |h| {
        h.download = DownloadState::Running {
            done: 0,
            total: u.size,
        };
        h.cancel = Some(d.cancel.clone());
    });
    let rx = d.events;
    window
        .spawn(cx, async move |cx| {
            while let Ok(ev) = rx.recv().await {
                let last = !matches!(ev, UpdateEvent::Progress { .. });
                if cx.update(|_, cx| apply_event(ev, cx)).is_err() || last {
                    break;
                }
            }
            // The sender went away without a result.
            let _ = cx.update(|_, cx| {
                if matches!(
                    hub(cx).map(|h| &h.download),
                    Some(DownloadState::Running { .. })
                ) {
                    apply_event(UpdateEvent::Failed(t("update-cancelled")), cx);
                }
            });
        })
        .detach();
}

/// Applies one download event.
pub fn apply_event(ev: UpdateEvent, cx: &mut App) {
    match &ev {
        UpdateEvent::Ready(p) => output::info(
            t_args(
                "update-downloaded",
                &[("path", p.display().to_string().into())],
            ),
            cx,
        ),
        UpdateEvent::Failed(e) => {
            output::log(output::Level::Warning, format!("update download: {e}"), cx)
        }
        UpdateEvent::Progress { .. } => {}
    }
    update_hub(cx, |h| {
        h.download = match ev {
            UpdateEvent::Progress { done, total } => DownloadState::Running { done, total },
            UpdateEvent::Ready(p) => DownloadState::Ready(p),
            UpdateEvent::Failed(e) => DownloadState::Failed(e),
        };
        if !matches!(h.download, DownloadState::Running { .. }) {
            h.cancel = None;
        }
    });
}

pub fn cancel_download(cx: &mut App) {
    if let Some(c) = hub(cx).and_then(|h| h.cancel.clone()) {
        c.cancel();
    }
}

/// Starts the installer of the verified package, then quits.
pub fn install_and_restart(window: &mut Window, cx: &mut App) {
    let Some(DownloadState::Ready(path)) = hub(cx).map(|h| h.download.clone()) else {
        return;
    };
    update_hub(cx, |h| h.installing = true);
    let backend = AppState::backend(cx);
    let task = cx
        .background_executor()
        .spawn(async move { backend.updates().install(path).await });
    window
        .spawn(cx, async move |cx| {
            let r = task.await;
            let _ = cx.update(|window, cx| {
                update_hub(cx, |h| h.installing = false);
                match r {
                    Ok(()) => {
                        output::info(t("update-installing"), cx);
                        window.dispatch_action(Box::new(crate::actions::Quit), cx);
                    }
                    Err(e) => window.push_notification(
                        Notification::error(t_args(
                            "update-install-failed",
                            &[("error", e.to_string().into())],
                        )),
                        cx,
                    ),
                }
            });
        })
        .detach();
}

/// Help ▸ Join Beta Program: switches to the beta channel and checks.
pub fn join_beta(window: &mut Window, cx: &mut App) {
    let backend = AppState::backend(cx);
    if !backend.updates().available() {
        window.push_notification(Notification::info(t("update-unavailable")), cx);
        return;
    }
    let mut s = backend.updates().settings();
    if s.channel == UpdateChannel::Beta {
        window.push_notification(Notification::info(t("beta-already")), cx);
        return;
    }
    s.channel = UpdateChannel::Beta;
    let task = cx
        .background_executor()
        .spawn(async move { backend.updates().save_settings(s).await });
    window
        .spawn(cx, async move |cx| {
            let r = task.await;
            let _ = cx.update(|window, cx| match r {
                Ok(()) => {
                    window.push_notification(Notification::success(t("beta-joined")), cx);
                    check(false, window, cx);
                }
                Err(e) => {
                    window.push_notification(Notification::error(e.to_string()), cx);
                }
            });
        })
        .detach();
}

// ------------------------------------------------------------------ help

/// Version, API version, channel, platform, backend and the last crash.
pub fn diagnostics(cx: &App) -> String {
    let backend = AppState::backend(cx);
    let channel = if backend.updates().available() {
        channel_label(backend.updates().settings().channel)
    } else {
        "—".into()
    };
    let mut text = format!(
        "FerroDb {}\nAPI v{API_VERSION}\n{}: {channel}\nOS: {} {}\n{}: {}\n",
        app_version(cx),
        t("about-channel-label"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        t("about-backend-label"),
        backend.name(),
    );
    if let Some(r) = backend.crash_reports().all().first() {
        text.push_str(&format!(
            "{}: {} {} ({})\n",
            t("crash-last"),
            r.id,
            r.message,
            r.location.clone().unwrap_or_default()
        ));
    }
    text
}

/// Help ▸ Report a Problem: copies the diagnostics and opens the tracker.
pub fn report_problem(window: &mut Window, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(diagnostics(cx)));
    window.push_notification(Notification::info(t("report-problem-copied")), cx);
    cx.open_url(ISSUES_URL);
}

/// Lines of the About dialog.
pub fn about_lines(cx: &App) -> Vec<String> {
    let backend = AppState::backend(cx);
    let mut lines = vec![
        t_args("about-version", &[("version", app_version(cx).into())]),
        t_args("about-api", &[("version", API_VERSION.into())]),
    ];
    if backend.updates().available() {
        lines.push(t_args(
            "about-channel",
            &[(
                "channel",
                channel_label(backend.updates().settings().channel).into(),
            )],
        ));
    }
    lines
}

// ------------------------------------------------------------------ crash dialog

pub struct CrashDialog {
    pub reports: Vec<CrashReport>,
    /// Reports whose details are shown.
    pub expanded: HashSet<String>,
    comment: Entity<InputState>,
    pub busy: bool,
    /// Sending failed (no endpoint in this build, network): copy / open
    /// folder are offered instead.
    pub error: Option<String>,
}

impl CrashDialog {
    fn new(reports: Vec<CrashReport>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let comment = cx.new(|cx| InputState::new(window, cx).placeholder(t("crash-comment")));
        Self {
            reports,
            expanded: HashSet::new(),
            comment,
            busy: false,
            error: None,
        }
    }

    fn ids(&self) -> Vec<String> {
        self.reports.iter().map(|r| r.id.clone()).collect()
    }

    pub fn toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_owned());
        }
        cx.notify();
    }

    pub fn set_comment(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.comment
            .update(cx, |i, cx| i.set_value(text.to_owned(), window, cx));
    }

    /// Sends the reports; `always` also sets the consent to Always.
    pub fn send(&mut self, always: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.ids();
        let comment = self.comment.read(cx).value().to_string();
        let backend = AppState::backend(cx);
        self.busy = true;
        self.error = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            if always {
                backend
                    .crash_reports()
                    .set_consent(CrashConsent::Always)
                    .await?;
            }
            backend.crash_reports().send(ids, comment).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match r {
                    Ok(()) => {
                        output::info(
                            t_args("crash-sent", &[("count", this.reports.len().into())]),
                            cx,
                        );
                        close_crash_dialog(window, cx);
                    }
                    Err(e) => {
                        output::log(output::Level::Warning, format!("crash reports: {e}"), cx);
                        this.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Keeps the reports on disk but stops offering them.
    pub fn dont_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.ids();
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.crash_reports().dismiss(ids).await });
        cx.spawn_in(window, async move |_, cx| {
            let r = task.await;
            let _ = cx.update(|window, cx| {
                if let Err(e) = r {
                    output::log(output::Level::Warning, format!("crash reports: {e}"), cx);
                }
                close_crash_dialog(window, cx);
            });
        })
        .detach();
    }

    /// The reports as JSON (to paste into an issue).
    pub fn report_text(&self) -> String {
        serde_json::to_string_pretty(&self.reports).unwrap_or_default()
    }
}

fn close_crash_dialog(window: &mut Window, cx: &mut App) {
    update_hub(cx, |h| h.crash_dialog = None);
    window.close_dialog(cx);
}

/// Copies every stored report (JSON) to the clipboard.
pub fn copy_reports(reports: &[CrashReport], window: &mut Window, cx: &mut App) {
    let text = serde_json::to_string_pretty(reports).unwrap_or_default();
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    window.push_notification(Notification::info(t("crash-copied")), cx);
}

/// Opens the reports folder in the file manager.
pub fn open_reports_folder(window: &mut Window, cx: &mut App) {
    match AppState::backend(cx).crash_reports().directory() {
        Some(dir) => {
            let _ = std::fs::create_dir_all(&dir);
            cx.open_with_system(&dir);
        }
        None => window.push_notification(Notification::info(t("crash-no-folder")), cx),
    }
}

impl Render for CrashDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let weak = cx.entity().downgrade();
        v_flex()
            .id("crash-dialog")
            .test_support()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        icons::Icon::from(icons::Lucide::TriangleAlert).text_color(theme.warning),
                    )
                    .child(div().text_sm().child(t("crash-intro"))),
            )
            .child(
                v_flex()
                    .id("crash-reports")
                    .max_h(px(260.))
                    .overflow_y_scroll()
                    .gap_1()
                    .children(self.reports.iter().map(|r| {
                        let open = self.expanded.contains(&r.id);
                        let id = r.id.clone();
                        let w = weak.clone();
                        v_flex()
                            .gap_1()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "crash-toggle-{}",
                                            r.id
                                        )))
                                        .ghost()
                                        .xsmall()
                                        .icon(if open {
                                            icons::Lucide::ChevronDown
                                        } else {
                                            icons::Lucide::ChevronRight
                                        })
                                        .tooltip(t("crash-details"))
                                        .on_click(
                                            move |_, _, cx| {
                                                let _ = w.update(cx, |v, cx| v.toggle(&id, cx));
                                            },
                                        ),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(
                                                r.message.lines().next().unwrap_or("").to_owned(),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(r.time.chars().take(19).collect::<String>()),
                                    ),
                            )
                            .when(open, |d| d.child(report_details(r, cx)))
                    })),
            )
            .child(Input::new(&self.comment).small())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("crash-privacy")),
            )
            .when_some(self.error.clone(), |d, e| {
                let reports = self.reports.clone();
                d.child(
                    v_flex()
                        .id("crash-send-error")
                        .test_support()
                        .gap_1()
                        .p_2()
                        .rounded_md()
                        .bg(theme.danger.opacity(0.1))
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.danger)
                                .child(t_args("crash-send-failed", &[("error", e.into())])),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("crash-copy")
                                        .small()
                                        .label(t("crash-copy"))
                                        .on_click(move |_, window, cx| {
                                            copy_reports(&reports, window, cx)
                                        }),
                                )
                                .child(
                                    Button::new("crash-open-folder")
                                        .small()
                                        .label(t("crash-open-folder"))
                                        .on_click(|_, window, cx| open_reports_folder(window, cx)),
                                ),
                        ),
                )
            })
    }
}

/// Details of one report: where, when, platform, backtrace and log tail.
fn report_details(r: &CrashReport, cx: &App) -> Div {
    let theme = cx.theme();
    let mut text = format!(
        "{}: {}\n{}: {}\n{}: {}\n{}: {}\n",
        t("crash-field-version"),
        r.app_version,
        t("crash-field-os"),
        r.os,
        t("crash-field-thread"),
        r.thread.clone().unwrap_or_else(|| "—".into()),
        t("crash-field-location"),
        r.location.clone().unwrap_or_else(|| "—".into()),
    );
    text.push_str(&format!(
        "\n{}\n{}\n",
        t("crash-field-backtrace"),
        r.backtrace
    ));
    if !r.log_tail.is_empty() {
        text.push_str(&format!(
            "\n{}\n{}\n",
            t("crash-field-log"),
            r.log_tail.join("\n")
        ));
    }
    div()
        .max_h(px(200.))
        .p_1()
        .rounded_sm()
        .bg(theme.muted)
        .font_family("monospace")
        .text_xs()
        .child(text)
}

/// Opens the dialog offering `reports` (consent Ask).
pub fn open_crash_dialog(reports: Vec<CrashReport>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| CrashDialog::new(reports, window, cx));
    update_hub(cx, |h| h.crash_dialog = Some(view.clone()));
    window.open_dialog(cx, move |dialog, _, cx| {
        let v = view.read(cx);
        let busy = v.busy;
        let (w1, w2, w3) = (view.downgrade(), view.downgrade(), view.downgrade());
        dialog
            .title(t("crash-title"))
            .w(px(620.))
            .keyboard(false)
            .overlay_closable(false)
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(
                        Button::new("crash-dont-send")
                            .label(t("crash-dont-send"))
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                let _ = w1.update(cx, |v, cx| v.dont_send(window, cx));
                            }),
                    )
                    .child(
                        Button::new("crash-always-send")
                            .label(t("crash-always-send"))
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                let _ = w2.update(cx, |v, cx| v.send(true, window, cx));
                            }),
                    )
                    .child(
                        Button::new("crash-send")
                            .primary()
                            .label(t("crash-send"))
                            .loading(busy)
                            .on_click(move |_, window, cx| {
                                let _ = w3.update(cx, |v, cx| v.send(false, window, cx));
                            }),
                    ),
            )
    });
}

// ------------------------------------------------------------------ settings page

/// "Updates and Privacy": channel, automatic checks, Check Now, crash
/// report consent and the stored reports. Every change is saved at once.
pub struct ReleaseSettingsView {
    pub settings: UpdateSettings,
    pub consent: CrashConsent,
    pub checking: bool,
    pub check: Option<Result<Option<UpdateInfo>, String>>,
    pub show_reports: bool,
    pub reports: Vec<CrashReport>,
    /// Report whose details are open.
    pub open_report: Option<String>,
    /// Outcome of the last save / clear.
    pub outcome: Option<Result<String, String>>,
}

impl ReleaseSettingsView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let backend = AppState::backend(cx);
        Self {
            settings: backend.updates().settings(),
            consent: backend.crash_reports().consent(),
            checking: false,
            check: None,
            show_reports: false,
            reports: Vec::new(),
            open_report: None,
            outcome: None,
        }
    }

    fn save_updates(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.updates().save_settings(settings).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.outcome = Some(r.map(|_| t("settings-saved")).map_err(|e| e.to_string()));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn set_channel(&mut self, channel: UpdateChannel, cx: &mut Context<Self>) {
        self.settings.channel = channel;
        self.check = None;
        self.save_updates(cx);
    }

    pub fn set_check_automatically(&mut self, v: bool, cx: &mut Context<Self>) {
        self.settings.check_automatically = v;
        self.save_updates(cx);
    }

    pub fn set_consent(&mut self, consent: CrashConsent, cx: &mut Context<Self>) {
        self.consent = consent;
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.crash_reports().set_consent(consent).await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.outcome = Some(r.map(|_| t("settings-saved")).map_err(|e| e.to_string()));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Checks now; the result is shown on the page (and in the status bar).
    pub fn check_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        self.checking = true;
        self.check = None;
        cx.notify();
        let task = cx
            .background_executor()
            .spawn(async move { backend.updates().check().await });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await.map_err(|e| e.to_string());
            let _ = this.update_in(cx, |this, _, cx| {
                this.checking = false;
                this.check = Some(r.clone());
                update_hub(cx, |h| {
                    h.last_check = Some(r.clone());
                    if let Ok(Some(u)) = &r
                        && h.update.as_ref().is_none_or(|old| old.version != u.version)
                    {
                        h.update = Some(u.clone());
                        h.download = DownloadState::Idle;
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub fn toggle_reports(&mut self, cx: &mut Context<Self>) {
        self.show_reports = !self.show_reports;
        if self.show_reports {
            self.reload_reports(cx);
        }
        cx.notify();
    }

    fn reload_reports(&mut self, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.crash_reports().all() });
        cx.spawn(async move |this, cx| {
            let reports = task.await;
            let _ = this.update(cx, |this, cx| {
                this.reports = reports;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn toggle_report(&mut self, id: &str, cx: &mut Context<Self>) {
        self.open_report = if self.open_report.as_deref() == Some(id) {
            None
        } else {
            Some(id.to_owned())
        };
        cx.notify();
    }

    pub fn clear_reports(&mut self, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.crash_reports().clear().await });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.outcome = Some(r.map(|_| t("crash-cleared")).map_err(|e| e.to_string()));
                this.reports.clear();
                this.open_report = None;
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ReleaseSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let backend = AppState::backend(cx);
        let theme = cx.theme().clone();
        let weak = cx.entity().downgrade();
        let updates = backend.updates().available();
        let crashes = backend.crash_reports().available();
        let check_text = match &self.check {
            Some(Ok(Some(u))) => Some(Ok(t_args(
                "update-available",
                &[("version", u.version.clone().into())],
            ))),
            Some(Ok(None)) => Some(Ok(t_args(
                "update-none",
                &[("version", app_version(cx).into())],
            ))),
            Some(Err(e)) => Some(Err(e.clone())),
            None => None,
        };
        let found = matches!(self.check, Some(Ok(Some(_))));
        v_flex()
            .id("release-settings")
            .test_support()
            .gap_2()
            .p_1()
            .child(section(t("updates-section"), cx))
            .child(div().text_sm().child(t_args(
                "about-version",
                &[("version", app_version(cx).into())],
            )))
            .when(!updates, |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t("update-unavailable")),
                )
            })
            .when(updates, |d| {
                d.child(field_row(
                    t("update-channel"),
                    choice(
                        "update-channel",
                        channel_label(self.settings.channel),
                        vec![
                            (UpdateChannel::Stable, channel_label(UpdateChannel::Stable)),
                            (UpdateChannel::Beta, channel_label(UpdateChannel::Beta)),
                        ],
                        weak.clone(),
                        |v: &mut Self, c, cx| v.set_channel(c, cx),
                    ),
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("update-beta-description")),
                )
                .child(field_row(
                    t("update-check-automatically"),
                    Switch::new("update-check-automatically")
                        .checked(self.settings.check_automatically)
                        .on_click(cx.listener(|this, v: &bool, _, cx| {
                            this.set_check_automatically(*v, cx)
                        })),
                    cx,
                ))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("update-check-now")
                                .small()
                                .label(t("update-check-now"))
                                .loading(self.checking)
                                .on_click(cx.listener(|this, _, w, cx| this.check_now(w, cx))),
                        )
                        .when(found, |d| {
                            d.child(
                                Button::new("update-view")
                                    .small()
                                    .primary()
                                    .label(t("update-view"))
                                    .on_click(|_, window, cx| open_update_dialog(window, cx)),
                            )
                        })
                        .children(check_text.as_ref().map(|o| outcome(o, cx))),
                )
            })
            .child(section(t("crash-section"), cx))
            .when(!crashes, |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t("crash-unavailable")),
                )
            })
            .when(crashes, |d| {
                d.child(field_row(
                    t("crash-consent"),
                    choice(
                        "crash-consent",
                        consent_label(self.consent),
                        [CrashConsent::Ask, CrashConsent::Always, CrashConsent::Never]
                            .into_iter()
                            .map(|c| (c, consent_label(c)))
                            .collect(),
                        weak.clone(),
                        |v: &mut Self, c, cx| v.set_consent(c, cx),
                    ),
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("crash-consent-description")),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("crash-view-reports")
                                .small()
                                .label(if self.show_reports {
                                    t("crash-hide-reports")
                                } else {
                                    t("crash-view-reports")
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_reports(cx))),
                        )
                        .child(
                            Button::new("crash-settings-open-folder")
                                .small()
                                .label(t("crash-open-folder"))
                                .on_click(|_, window, cx| open_reports_folder(window, cx)),
                        )
                        .child(
                            Button::new("crash-clear")
                                .small()
                                .danger()
                                .label(t("crash-clear"))
                                .on_click(cx.listener(|this, _, _, cx| this.clear_reports(cx))),
                        ),
                )
                .when(self.show_reports, |d| d.child(self.render_reports(cx)))
            })
            .children(self.outcome.as_ref().map(|o| outcome(o, cx)))
            .child(
                div()
                    .pt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(t("privacy-no-telemetry")),
            )
    }
}

impl ReleaseSettingsView {
    fn render_reports(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let weak = cx.entity().downgrade();
        v_flex()
            .id("crash-report-list")
            .test_support()
            .gap_1()
            .max_h(px(260.))
            .overflow_y_scroll()
            .when(self.reports.is_empty(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t("crash-none")),
                )
            })
            .children(self.reports.iter().map(|r| {
                let open = self.open_report.as_deref() == Some(r.id.as_str());
                let (id, w) = (r.id.clone(), weak.clone());
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(
                                Button::new(SharedString::from(format!("crash-report-{}", r.id)))
                                    .ghost()
                                    .xsmall()
                                    .icon(if open {
                                        icons::Lucide::ChevronDown
                                    } else {
                                        icons::Lucide::ChevronRight
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = w.update(cx, |v, cx| v.toggle_report(&id, cx));
                                    }),
                            )
                            .child(
                                div()
                                    .text_color(theme.muted_foreground)
                                    .child(r.time.chars().take(19).collect::<String>()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(r.message.lines().next().unwrap_or("").to_owned()),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).child(
                                if r.sent {
                                    t("crash-status-sent")
                                } else {
                                    t("crash-status-local")
                                },
                            )),
                    )
                    .when(open, |d| d.child(report_details(r, cx)))
            }))
    }
}
