use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        ActiveTheme, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex,
        kbd::Kbd,
        v_flex,
    },
};

use super::panel_boilerplate;
use crate::{
    actions::{self, ACTIONS},
    i18n::t,
};

pub const NAME: &str = "Welcome";

pub struct WelcomePanel {
    focus: FocusHandle,
}

impl WelcomePanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
        }
    }
}

panel_boilerplate!(WelcomePanel);

impl BasePanel for WelcomePanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for WelcomePanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-welcome")
    }
}

fn shortcut_row(label: String, keys: Option<AnyElement>, cx: &App) -> impl IntoElement {
    h_flex()
        .w(px(420.))
        .justify_between()
        .py_1()
        .child(div().text_color(cx.theme().foreground).child(label))
        .children(keys)
}

impl Render for WelcomePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let featured = [
            "action-search-everywhere",
            "action-find-action",
            "action-new-console",
            "action-execute",
            "action-open-settings",
        ];
        let rows = featured
            .iter()
            .filter_map(|id| ACTIONS.iter().find(|a| a.label_id == *id))
            .map(|spec| {
                let action = (spec.build)();
                let keys = if spec.label_id == "action-search-everywhere" {
                    Some(div().child(t("keys-double-shift")).into_any_element())
                } else {
                    Kbd::binding_for_action(action.as_ref(), spec.context, window)
                        .map(|k| k.into_any_element())
                };
                shortcut_row(t(spec.label_id), keys, cx)
            });
        v_flex()
            .id("welcome")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("app-name")),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child(t("welcome-subtitle")),
            )
            .child(v_flex().children(rows))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("welcome-new-console")
                            .primary()
                            .small()
                            .label(t("action-new-console"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(actions::NewConsole), cx)
                            }),
                    )
                    .child(
                        Button::new("welcome-open-project")
                            .small()
                            .label(t("action-open-project"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(actions::OpenProject), cx)
                            }),
                    ),
            )
    }
}
