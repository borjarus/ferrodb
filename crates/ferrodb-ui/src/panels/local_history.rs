//! Local History panel (plan, phase 7): documents with revisions (consoles
//! and project files) on the left, the revisions of the selected one on the
//! right. A revision can be compared with the current text, restored (the
//! current text is kept as a revision first), labelled or deleted. The
//! revisions live in the backend (`app.db` with the core) and survive
//! restarts.

use ferrodb_api::{DataSourceId, HistoryDocument, Revision};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex,
        input::{Input, InputState},
        notification::Notification,
        v_flex,
    },
};

use super::{ConsolePanel, history::time_of_day, panel_boilerplate};
use crate::{
    AppState, activity,
    i18n::t,
    local_history::{document_label, reason_label},
    views::diff::DiffView,
    workspace,
};

pub const NAME: &str = "LocalHistory";

pub struct LocalHistoryPanel {
    focus: FocusHandle,
    pub documents: Vec<HistoryDocument>,
    pub document: Option<String>,
    pub revisions: Vec<Revision>,
    pub selected: Option<i64>,
    label: Entity<InputState>,
    _sub: Option<Subscription>,
}

/// The open console editing `document`, if any.
fn console_of(document: &str, cx: &App) -> Option<Entity<ConsolePanel>> {
    workspace::active(cx)?
        .read(cx)
        .consoles()
        .into_iter()
        .find(|c| c.read(cx).history_document() == document)
}

/// The current text of a document: its open console, else the file.
fn current_text(document: &str, cx: &App) -> Option<String> {
    if let Some(c) = console_of(document, cx) {
        return Some(c.read(cx).editor().read(cx).value().to_string());
    }
    if document.starts_with("console:") {
        return None;
    }
    std::fs::read_to_string(document).ok()
}

impl LocalHistoryPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut last = 0;
        let sub = activity::entity(cx).map(|e| {
            cx.observe(&e, move |this: &mut Self, e, cx| {
                let n = e.read(cx).revisions;
                if n != last {
                    last = n;
                    this.reload(cx);
                }
            })
        });
        let label = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t("local-history-label-placeholder"))
        });
        let mut this = Self {
            focus: cx.focus_handle(),
            documents: Vec::new(),
            document: None,
            revisions: Vec::new(),
            selected: None,
            label,
            _sub: sub,
        };
        this.reload(cx);
        this
    }

    /// Reloads the documents and the revisions of the selected one.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let doc = self.document.clone();
        let task = cx.background_executor().spawn(async move {
            let h = backend.local_history();
            let docs = h.documents().await.unwrap_or_default();
            let doc = doc.or_else(|| docs.first().map(|d| d.document.clone()));
            let revs = match &doc {
                Some(d) => h.revisions(d.clone()).await.unwrap_or_default(),
                None => Vec::new(),
            };
            (docs, doc, revs)
        });
        cx.spawn(async move |this, cx| {
            let (docs, doc, revs) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.documents = docs;
                this.document = doc;
                this.revisions = revs;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn select_document(&mut self, document: String, cx: &mut Context<Self>) {
        self.document = Some(document);
        self.selected = None;
        self.reload(cx);
    }

    fn compare(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.document.clone() else {
            return;
        };
        let rev = self.revisions.iter().find(|r| r.id == id).cloned();
        let current = current_text(&doc, cx).unwrap_or_default();
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.local_history().text(id).await });
        cx.spawn_in(window, async move |_, cx| {
            let text = task.await;
            let _ = cx.update(|window, cx| match text {
                Ok(text) => {
                    let at = rev.map(|r| time_of_day(r.at)).unwrap_or_default();
                    let view = cx.new(|cx| {
                        DiffView::new(
                            format!("{} {at}", t("local-history-snapshot")),
                            &text,
                            t("local-history-current"),
                            &current,
                            "sql",
                            cx,
                        )
                    });
                    window.open_dialog(cx, move |dialog, _, _| {
                        dialog
                            .title(t("action-local-history"))
                            .w(px(900.))
                            .child(div().h(px(480.)).child(view.clone()))
                    });
                }
                Err(e) => window.push_notification(Notification::error(e.to_string()), cx),
            });
        })
        .detach();
    }

    /// Puts the revision's text back: into the open console of the
    /// document, else a new console (bound to the file for file documents).
    pub fn restore(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.document.clone() else {
            return;
        };
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.local_history().text(id).await });
        cx.spawn_in(window, async move |_, cx| {
            let text = task.await;
            let _ = cx.update(|window, cx| {
                let text = match text {
                    Ok(t) => t,
                    Err(e) => {
                        window.push_notification(Notification::error(e.to_string()), cx);
                        return;
                    }
                };
                if let Some(c) = console_of(&doc, cx) {
                    c.update(cx, |c, cx| c.set_text(text, window, cx));
                } else if let Some(ds) = doc.strip_prefix("console:") {
                    let ds = (!ds.is_empty()).then(|| DataSourceId::new(ds));
                    workspace::open_console(ds, text, window, cx);
                } else {
                    let path = std::path::PathBuf::from(&doc);
                    let ds = workspace::active_console_source(cx);
                    if path.exists()
                        && workspace::open_file_console(ds.clone(), path, window, cx).is_ok()
                    {
                        if let Some(c) = console_of(&doc, cx) {
                            c.update(cx, |c, cx| c.set_text(text, window, cx));
                        }
                    } else {
                        workspace::open_console(ds, text, window, cx);
                    }
                }
                window.push_notification(Notification::success(t("local-history-restored")), cx);
            });
        })
        .detach();
    }

    fn delete(&mut self, id: i64, cx: &mut Context<Self>) {
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.local_history().delete(id).await });
        cx.spawn(async move |this, cx| {
            let _ = task.await;
            let _ = this.update(cx, |this, cx| this.reload(cx));
        })
        .detach();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.document.take() else {
            return;
        };
        let backend = AppState::backend(cx);
        let task = cx
            .background_executor()
            .spawn(async move { backend.local_history().clear(doc).await });
        cx.spawn(async move |this, cx| {
            let _ = task.await;
            let _ = this.update(cx, |this, cx| this.reload(cx));
        })
        .detach();
    }

    /// Labels the current text of the selected document (or of the most
    /// recently used console).
    pub fn put_label(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.label.read(cx).value().trim().to_owned();
        let doc = self.document.clone().or_else(|| {
            workspace::active(cx)?
                .read(cx)
                .consoles()
                .last()
                .map(|c| c.read(cx).history_document())
        });
        let Some(doc) = doc else { return };
        let Some(text) = current_text(&doc, cx) else {
            window.push_notification(Notification::info(t("local-history-not-open")), cx);
            return;
        };
        crate::local_history::record(
            doc.clone(),
            text,
            ferrodb_api::RevisionReason::Label,
            (!label.is_empty()).then_some(label),
            cx,
        );
        self.document = Some(doc);
        self.label.update(cx, |i, cx| i.set_value("", window, cx));
    }
}

panel_boilerplate!(LocalHistoryPanel);

impl BasePanel for LocalHistoryPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for LocalHistoryPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-local-history")
    }
}

impl Render for LocalHistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let docs = self
            .documents
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let selected = self.document.as_deref() == Some(d.document.as_str());
                let doc = d.document.clone();
                h_flex()
                    .id(("lh-doc", i))
                    .px_2()
                    .py_0p5()
                    .gap_2()
                    .when(selected, |x| x.bg(theme.accent))
                    .hover(|x| x.bg(theme.muted))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select_document(doc.clone(), cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(document_label(&d.document)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(d.revisions.to_string()),
                    )
            })
            .collect::<Vec<_>>();
        let revs = self
            .revisions
            .iter()
            .map(|r| {
                let id = r.id;
                let selected = self.selected == Some(id);
                h_flex()
                    .id(("local-history", id as usize))
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .when(selected, |d| d.bg(theme.accent))
                    .hover(|s| s.bg(theme.muted))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(id);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w(px(64.))
                            .text_color(theme.muted_foreground)
                            .child(time_of_day(r.at)),
                    )
                    .child(
                        div()
                            .w(px(110.))
                            .text_color(theme.muted_foreground)
                            .child(t(reason_label(r.reason))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .when(r.reason == ferrodb_api::RevisionReason::Label, |d| {
                                d.font_weight(FontWeight::SEMIBOLD)
                            })
                            .child(r.label.clone().unwrap_or_default()),
                    )
                    .child(
                        div()
                            .w(px(70.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} B", r.size)),
                    )
                    .child(
                        Button::new(("lh-compare", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::GitCompare)
                            .tooltip(t("local-history-compare"))
                            .on_click(cx.listener(move |this, _, w, cx| this.compare(id, w, cx))),
                    )
                    .child(
                        Button::new(("lh-restore", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::Undo2)
                            .tooltip(t("local-history-restore"))
                            .on_click(cx.listener(move |this, _, w, cx| this.restore(id, w, cx))),
                    )
                    .child(
                        Button::new(("lh-delete", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::Trash)
                            .tooltip(t("local-history-delete"))
                            .on_click(cx.listener(move |this, _, _, cx| this.delete(id, cx))),
                    )
            })
            .collect::<Vec<_>>();
        let empty = self.documents.is_empty();
        v_flex()
            .id("local-history")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .text_sm()
            .child(
                h_flex()
                    .px_1()
                    .py_0p5()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("lh-refresh")
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::RefreshCw)
                            .tooltip(t("files-refresh"))
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    )
                    .child(div().w(px(200.)).child(Input::new(&self.label).xsmall()))
                    .child(
                        Button::new("lh-label")
                            .ghost()
                            .xsmall()
                            .icon(icons::Lucide::Tag)
                            .label(t("local-history-put-label"))
                            .on_click(cx.listener(|this, _, w, cx| this.put_label(w, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("lh-clear")
                            .ghost()
                            .xsmall()
                            .label(t("local-history-clear"))
                            .disabled(self.document.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                    ),
            )
            .when(empty, |d| {
                d.child(
                    div()
                        .p_2()
                        .text_color(theme.muted_foreground)
                        .child(t("local-history-empty")),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("lh-docs")
                            .w(px(220.))
                            .h_full()
                            .border_r_1()
                            .border_color(theme.border)
                            .overflow_y_scroll()
                            .children(docs),
                    )
                    .child(
                        div()
                            .id("lh-rows")
                            .flex_1()
                            .h_full()
                            .overflow_y_scroll()
                            .children(revs),
                    ),
            )
    }
}
