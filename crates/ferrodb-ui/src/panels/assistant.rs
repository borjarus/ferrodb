//! AI Assistant tool window (plan 2.7, phase 9): a chat about the data source
//! of the active console. Answers stream in; suggested queries can be
//! inserted into a console, opened in a new one or copied. Nothing is ever
//! run by the assistant.

use ferrodb_api::{
    AssistantEvent, AssistantRequest, AssistantTask, CancelHandle, ChatRole, ChatTurn,
    DataSourceId, SuggestedQuery,
};
use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Disableable as _, Sizable as _, WindowExt as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex,
        input::{InputEvent, Textarea, TextareaState},
        menu::{DropdownMenu as _, PopupMenuItem},
        notification::Notification,
        tag::Tag,
        v_flex,
    },
};

use super::panel_boilerplate;
use crate::{
    AppState, actions,
    i18n::{t, t_args},
    output, workspace,
};

pub const NAME: &str = "AiAssistant";

/// The assistant panel of the active workspace (tests, console actions).
pub struct ActiveAssistant(pub WeakEntity<AssistantPanel>);
impl Global for ActiveAssistant {}

/// Whether the assistant can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantState {
    /// Not compiled into this build.
    Unavailable,
    /// Turned off in the AI settings.
    Disabled,
    Ready,
}

pub fn state(cx: &App) -> AssistantState {
    let backend = AppState::backend(cx);
    let a = backend.assistant();
    if !a.available() {
        AssistantState::Unavailable
    } else if !a.settings().enabled {
        AssistantState::Disabled
    } else {
        AssistantState::Ready
    }
}

/// A piece of a Markdown answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Text(String),
    Code { lang: String, text: String },
}

/// Splits Markdown into text and fenced code blocks; an unterminated fence
/// (still streaming) counts as code.
pub fn segments(md: &str) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut code: Option<(String, String)> = None;
    for line in md.split_inclusive('\n') {
        let trimmed = line.trim();
        match &mut code {
            Some((lang, body)) => {
                if trimmed.starts_with("```") {
                    out.push(Segment::Code {
                        lang: std::mem::take(lang),
                        text: body.trim_end_matches('\n').to_owned(),
                    });
                    code = None;
                } else {
                    body.push_str(line);
                }
            }
            None => {
                if let Some(lang) = trimmed.strip_prefix("```") {
                    if !text.trim().is_empty() {
                        out.push(Segment::Text(text.trim().to_owned()));
                    }
                    text.clear();
                    code = Some((lang.trim().to_owned(), String::new()));
                } else {
                    text.push_str(line);
                }
            }
        }
    }
    if let Some((lang, body)) = code {
        out.push(Segment::Code {
            lang,
            text: body.trim_end_matches('\n').to_owned(),
        });
    }
    if !text.trim().is_empty() {
        out.push(Segment::Text(text.trim().to_owned()));
    }
    out
}

/// `8.4k` for character counts.
pub fn short_count(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// One question and its answer.
#[derive(Clone, Debug)]
pub struct Exchange {
    pub task: AssistantTask,
    pub prompt: String,
    /// Query text the question is about.
    pub context: Option<String>,
    /// What was sent along: (objects, characters).
    pub sent: Option<(usize, usize)>,
    pub answer: String,
    pub queries: Vec<SuggestedQuery>,
    pub error: Option<String>,
    pub done: bool,
    pub stopped: bool,
    pub model: Option<String>,
}

impl Exchange {
    /// The user's turn as sent in later requests.
    fn user_text(&self) -> String {
        match &self.context {
            Some(c) => format!("{}\n\n```\n{c}\n```", self.prompt),
            None => self.prompt.clone(),
        }
    }
}

fn task_label(task: &AssistantTask) -> Option<String> {
    match task {
        AssistantTask::Explain => Some(t("ai-task-explain")),
        AssistantTask::Optimize => Some(t("ai-task-optimize")),
        AssistantTask::Fix { .. } => Some(t("ai-task-fix")),
        AssistantTask::Generate | AssistantTask::Chat => None,
    }
}

pub struct AssistantPanel {
    focus: FocusHandle,
    prompt: Entity<TextareaState>,
    /// Chosen data source; `None` with `pinned == false` follows the active
    /// console.
    source: Option<DataSourceId>,
    schema: Option<String>,
    pinned: bool,
    pub exchanges: Vec<Exchange>,
    running: Option<CancelHandle>,
    listen: Option<Task<()>>,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

impl AssistantPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder(t("ai-prompt-placeholder"))
        });
        let subs = vec![
            cx.subscribe_in(&prompt, window, |this, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.send(window, cx);
                }
            }),
            // Settings changes (enabled / provider) change the empty state.
            cx.observe_global::<crate::agents::AiSettingsRevision>(|_, cx| cx.notify()),
        ];
        cx.set_global(ActiveAssistant(cx.entity().downgrade()));
        Self {
            focus: cx.focus_handle(),
            prompt,
            source: None,
            schema: None,
            pinned: false,
            exchanges: Vec::new(),
            running: None,
            listen: None,
            scroll: ScrollHandle::new(),
            _subs: subs,
        }
    }

    /// Data source and schema the next question is about.
    pub fn target(&self, cx: &App) -> (Option<DataSourceId>, Option<String>) {
        if self.pinned {
            return (self.source.clone(), self.schema.clone());
        }
        let c = cx
            .try_global::<workspace::LastConsole>()
            .and_then(|c| c.0.upgrade());
        match c {
            Some(c) => {
                let c = c.read(cx);
                (c.data_source().cloned(), c.schema().cloned())
            }
            None => (None, None),
        }
    }

    /// Chooses the data source explicitly (`None` = no database context).
    pub fn set_target(
        &mut self,
        source: Option<DataSourceId>,
        schema: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.source = source;
        self.schema = schema;
        self.pinned = true;
        cx.notify();
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn prompt_text(&self, cx: &App) -> String {
        self.prompt.read(cx).value().to_string()
    }

    pub fn set_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt
            .update(cx, |p, cx| p.set_value(text.to_owned(), window, cx));
    }

    pub fn focus_prompt(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt.update(cx, |p, cx| p.focus(window, cx));
    }

    /// Sends the prompt typed in the input as a new question.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.prompt_text(cx).trim().to_owned();
        if text.is_empty() || self.running.is_some() {
            return;
        }
        self.prompt
            .update(cx, |p, cx| p.set_value(String::new(), window, cx));
        let task = if self.exchanges.is_empty() {
            AssistantTask::Generate
        } else {
            AssistantTask::Chat
        };
        self.ask(task, text, None, cx);
    }

    /// Earlier turns of this conversation (completed exchanges only).
    pub fn history(&self) -> Vec<ChatTurn> {
        self.exchanges
            .iter()
            .filter(|e| e.done && e.error.is_none() && !e.stopped)
            .flat_map(|e| {
                [
                    ChatTurn {
                        role: ChatRole::User,
                        text: e.user_text(),
                    },
                    ChatTurn {
                        role: ChatRole::Assistant,
                        text: e.answer.clone(),
                    },
                ]
            })
            .collect()
    }

    /// Asks the assistant; the answer streams into a new exchange.
    pub fn ask(
        &mut self,
        task: AssistantTask,
        prompt: String,
        context: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if state(cx) != AssistantState::Ready {
            cx.notify();
            return;
        }
        self.stop(cx);
        let (data_source, schema) = self.target(cx);
        let request = AssistantRequest {
            data_source,
            schema,
            task: task.clone(),
            prompt: prompt.clone(),
            context: context.clone(),
            history: self.history(),
        };
        self.exchanges.push(Exchange {
            task,
            prompt,
            context,
            sent: None,
            answer: String::new(),
            queries: Vec::new(),
            error: None,
            done: false,
            stopped: false,
            model: None,
        });
        let ix = self.exchanges.len() - 1;
        let handle = AppState::backend(cx).assistant().ask(request);
        self.running = Some(handle.cancel.clone());
        let events = handle.events;
        self.listen = Some(cx.spawn(async move |this, cx| {
            while let Ok(ev) = events.recv().await {
                let alive = this.update(cx, |this, cx| this.on_event(ix, ev, cx));
                if alive.is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |this, cx| {
                if let Some(e) = this.exchanges.get_mut(ix)
                    && !e.done
                {
                    e.done = true;
                }
                this.running = None;
                cx.notify();
            });
        }));
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn on_event(&mut self, ix: usize, ev: AssistantEvent, cx: &mut Context<Self>) {
        let Some(e) = self.exchanges.get_mut(ix) else {
            return;
        };
        match ev {
            AssistantEvent::Context { objects, chars } => e.sent = Some((objects.len(), chars)),
            AssistantEvent::Delta(d) => e.answer.push_str(&d),
            AssistantEvent::Finished(a) => {
                if !a.text.is_empty() {
                    e.answer = a.text;
                }
                e.queries = a.queries;
                e.model = (!a.model.is_empty()).then_some(a.model);
                e.done = true;
                self.running = None;
            }
            AssistantEvent::Failed(msg) => {
                output::log(output::Level::Error, format!("AI: {msg}"), cx);
                e.error = Some(msg);
                e.done = true;
                self.running = None;
            }
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Stops the answer being streamed.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.running.take() {
            c.cancel();
            self.listen = None;
            if let Some(e) = self.exchanges.last_mut()
                && !e.done
            {
                e.done = true;
                e.stopped = true;
            }
            cx.notify();
        }
    }

    /// Starts a new conversation.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        self.exchanges.clear();
        cx.notify();
    }

    fn query(&self, ex: usize, q: usize) -> Option<&SuggestedQuery> {
        self.exchanges.get(ex)?.queries.get(q)
    }

    /// Inserts a suggested query into the last used console, at the cursor
    /// or replacing its selection (a new console when none is open).
    pub fn insert_query(&mut self, ex: usize, q: usize, window: &mut Window, cx: &mut App) {
        let Some(text) = self.query(ex, q).map(|q| q.text.clone()) else {
            return;
        };
        let console = cx
            .try_global::<workspace::LastConsole>()
            .and_then(|c| c.0.upgrade());
        match console {
            Some(c) => {
                let editor = c.read(cx).editor().clone();
                editor.update(cx, |e, cx| {
                    e.replace(text, window, cx);
                    e.focus(window, cx);
                });
            }
            None => {
                let (ds, _) = self.target(cx);
                workspace::open_console(ds, text, window, cx);
            }
        }
    }

    pub fn open_query(&mut self, ex: usize, q: usize, window: &mut Window, cx: &mut App) {
        let Some(text) = self.query(ex, q).map(|q| q.text.clone()) else {
            return;
        };
        let (ds, _) = self.target(cx);
        workspace::open_console(ds, text, window, cx);
    }

    pub fn copy_query(&mut self, ex: usize, q: usize, window: &mut Window, cx: &mut App) {
        if let Some(text) = self.query(ex, q).map(|q| q.text.clone()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            window.push_notification(Notification::info(t("ai-copied")), cx);
        }
    }

    fn render_source_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (ds, schema) = self.target(cx);
        let sources = AppState::backend(cx).data_sources().list();
        let name = ds.as_ref().and_then(|id| {
            sources
                .iter()
                .find(|s| &s.config.id == id)
                .map(|s| s.config.name.clone())
        });
        let label = match (name, schema) {
            (Some(n), Some(s)) => format!("{n} · {s}"),
            (Some(n), None) => n,
            (None, _) => t("ai-no-source"),
        };
        let weak = cx.entity().downgrade();
        Button::new("ai-source")
            .ghost()
            .xsmall()
            .icon(icons::Lucide::Database)
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                let w = weak.clone();
                let menu = menu.item(PopupMenuItem::new(t("ai-follow-console")).on_click(
                    move |_, _, cx| {
                        let _ = w.update(cx, |this, cx| {
                            this.pinned = false;
                            cx.notify();
                        });
                    },
                ));
                let w = weak.clone();
                let menu = menu.item(PopupMenuItem::new(t("ai-no-source")).on_click(
                    move |_, _, cx| {
                        let _ = w.update(cx, |this, cx| this.set_target(None, None, cx));
                    },
                ));
                sources.iter().fold(menu.separator(), |menu, s| {
                    let (id, weak) = (s.config.id.clone(), weak.clone());
                    menu.item(PopupMenuItem::new(s.config.name.clone()).on_click(
                        move |_, _, cx| {
                            let _ = weak
                                .update(cx, |this, cx| this.set_target(Some(id.clone()), None, cx));
                        },
                    ))
                })
            })
    }

    fn render_empty(&self, st: AssistantState, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("ai-empty")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .child(
                icons::Icon::from(icons::Lucide::Sparkles)
                    .large()
                    .text_color(muted),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .text_center()
                    .child(match st {
                        AssistantState::Unavailable => t("ai-unavailable"),
                        _ => t("ai-disabled"),
                    }),
            )
            .when(st == AssistantState::Disabled, |d| {
                d.child(
                    Button::new("ai-open-settings")
                        .small()
                        .primary()
                        .label(t("action-ai-settings"))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(actions::OpenAiSettings), cx)
                        }),
                )
            })
            .into_any_element()
    }

    fn render_query(
        &self,
        ex: usize,
        q: usize,
        query: &SuggestedQuery,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let n = ex * 1000 + q;
        let weak = cx.entity().downgrade();
        let (w1, w2, w3) = (weak.clone(), weak.clone(), weak);
        v_flex()
            .gap_1()
            .when(query.modifies || !query.risks.is_empty(), |d| {
                d.child(
                    h_flex()
                        .id(("ai-modifies", n))
                        .test_support()
                        .gap_1()
                        .flex_wrap()
                        .child(Tag::danger().xsmall().child(t("ai-modifies")))
                        .children(query.risks.iter().map(|r| {
                            Tag::warning()
                                .xsmall()
                                .child(crate::panels::console::risk_label(*r))
                        })),
                )
            })
            .children(query.problems.iter().map(|p| {
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(t_args("ai-problem", &[("message", p.clone().into())]))
            }))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new(("ai-insert", n))
                            .xsmall()
                            .icon(icons::Lucide::TextCursorInput)
                            .label(t("ai-insert"))
                            .on_click(move |_, window, cx| {
                                let _ =
                                    w1.update(cx, |this, cx| this.insert_query(ex, q, window, cx));
                            }),
                    )
                    .child(
                        Button::new(("ai-open", n))
                            .xsmall()
                            .ghost()
                            .icon(icons::Lucide::SquareTerminal)
                            .label(t("ai-open-console"))
                            .on_click(move |_, window, cx| {
                                let _ =
                                    w2.update(cx, |this, cx| this.open_query(ex, q, window, cx));
                            }),
                    )
                    .child(
                        Button::new(("ai-copy", n))
                            .xsmall()
                            .ghost()
                            .icon(icons::Lucide::Copy)
                            .label(t("ai-copy"))
                            .on_click(move |_, window, cx| {
                                let _ =
                                    w3.update(cx, |this, cx| this.copy_query(ex, q, window, cx));
                            }),
                    ),
            )
    }

    fn render_exchange(&self, ix: usize, e: &Exchange, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let code = |text: String| {
            div()
                .w_full()
                .p_2()
                .rounded_md()
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                .child(text)
        };
        let user = v_flex()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(theme.secondary)
            .when_some(task_label(&e.task), |d, l| {
                d.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(l),
                )
            })
            .child(div().text_sm().child(e.prompt.clone()))
            .when_some(e.context.clone(), |d, c| d.child(code(c)));

        // Suggested queries go under the code block with the same text;
        // the rest are listed after the answer.
        let mut used = vec![false; e.queries.len()];
        let mut body: Vec<AnyElement> = Vec::new();
        for seg in segments(&e.answer) {
            match seg {
                Segment::Text(s) => {
                    for p in s.split("\n\n").filter(|p| !p.trim().is_empty()) {
                        body.push(
                            div()
                                .text_sm()
                                .child(p.trim().to_owned())
                                .into_any_element(),
                        );
                    }
                }
                Segment::Code { text, .. } => {
                    let hit = e
                        .queries
                        .iter()
                        .enumerate()
                        .find(|(i, q)| !used[*i] && q.text.trim() == text.trim())
                        .map(|(i, _)| i);
                    body.push(code(text).into_any_element());
                    if let Some(i) = hit {
                        used[i] = true;
                        body.push(
                            self.render_query(ix, i, &e.queries[i], cx)
                                .into_any_element(),
                        );
                    }
                }
            }
        }
        for (i, q) in e.queries.iter().enumerate().filter(|(i, _)| !used[*i]) {
            body.push(code(q.text.clone()).into_any_element());
            body.push(self.render_query(ix, i, q, cx).into_any_element());
        }

        let pending = !e.done && e.answer.is_empty();
        v_flex()
            .id(("ai-exchange", ix))
            .test_support()
            .gap_2()
            .child(user)
            .when_some(e.sent, |d, (objects, chars)| {
                d.child(
                    div()
                        .id(("ai-context", ix))
                        .test_support()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t_args(
                            "ai-context-sent",
                            &[
                                ("objects", objects.into()),
                                ("chars", short_count(chars).into()),
                            ],
                        )),
                )
            })
            .when(pending, |d| {
                d.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(t("ai-thinking")),
                )
            })
            .children(body)
            .when(e.stopped, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(t("ai-stopped")),
                )
            })
            .when_some(e.error.clone(), |d, err| {
                d.child(
                    div()
                        .id(("ai-error", ix))
                        .test_support()
                        .p_2()
                        .rounded_md()
                        .bg(theme.danger.opacity(0.12))
                        .text_color(theme.danger)
                        .text_sm()
                        .child(err),
                )
            })
            .when_some(e.model.clone(), |d, m| {
                d.child(div().text_xs().text_color(theme.muted_foreground).child(m))
            })
            .into_any_element()
    }
}

panel_boilerplate!(AssistantPanel);

impl BasePanel for AssistantPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for AssistantPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(icons::Icon::from(icons::Lucide::Sparkles).small())
            .child(t("panel-ai-assistant"))
    }
}

impl Render for AssistantPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let st = state(cx);
        let root = v_flex()
            .id("ai-assistant")
            .test_support()
            .track_focus(&self.focus)
            .size_full();
        if st != AssistantState::Ready {
            return root.child(self.render_empty(st, cx));
        }
        let running = self.running.is_some();
        let theme = cx.theme().clone();
        let exchanges: Vec<AnyElement> = self
            .exchanges
            .clone()
            .iter()
            .enumerate()
            .map(|(i, e)| self.render_exchange(i, e, cx))
            .collect();
        root.child(
            h_flex()
                .gap_1()
                .px_1()
                .py_0p5()
                .border_b_1()
                .border_color(theme.border)
                .child(self.render_source_picker(cx))
                .child(div().flex_1())
                .child(
                    Button::new("ai-stop")
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::Square)
                        .tooltip(t("ai-stop"))
                        .disabled(!running)
                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                )
                .child(
                    Button::new("ai-new-chat")
                        .ghost()
                        .xsmall()
                        .icon(icons::Lucide::MessageSquarePlus)
                        .tooltip(t("ai-new-chat"))
                        .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                ),
        )
        .child(
            v_flex()
                .id("ai-chat")
                .test_support()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p_2()
                .gap_3()
                .when(exchanges.is_empty(), |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(t("ai-hint")),
                    )
                })
                .children(exchanges),
        )
        .child(
            h_flex()
                .gap_1()
                .p_1()
                .items_end()
                .border_t_1()
                .border_color(theme.border)
                .child(div().flex_1().child(Textarea::new(&self.prompt).small()))
                .child(
                    Button::new("ai-send")
                        .small()
                        .primary()
                        .icon(icons::Lucide::SendHorizontal)
                        .tooltip(t("ai-send"))
                        .loading(running)
                        .on_click(cx.listener(|this, _, w, cx| this.send(w, cx))),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn markdown_segments() {
        let md = "Here you go:\n\n```sql\nSELECT 1;\n```\nMore text.\n\n```sql\nSELECT";
        assert_eq!(
            segments(md),
            vec![
                Segment::Text("Here you go:".into()),
                Segment::Code {
                    lang: "sql".into(),
                    text: "SELECT 1;".into()
                },
                Segment::Text("More text.".into()),
                Segment::Code {
                    lang: "sql".into(),
                    text: "SELECT".into()
                },
            ]
        );
        assert_eq!(short_count(8400), "8.4k");
        assert_eq!(short_count(512), "512");
    }
}
