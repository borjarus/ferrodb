//! Inspector: the value panel of the selected cell / field / key and the
//! properties of the selected explorer object (see [`crate::inspector`]).

use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    icons,
    kit::{
        ActiveTheme, Sizable as _,
        button::{Button, ButtonVariants as _},
        dock::{BasePanel, Panel},
        h_flex, v_flex,
    },
};

use super::panel_boilerplate;
use crate::{
    i18n::t,
    inspector::{self, Inspection},
};

pub const NAME: &str = "Inspector";

pub struct InspectorPanel {
    focus: FocusHandle,
    _sub: Option<Subscription>,
}

impl InspectorPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sub = inspector::entity(cx).map(|e| cx.observe(&e, |_, _, cx| cx.notify()));
        Self {
            focus: cx.focus_handle(),
            _sub: sub,
        }
    }
}

panel_boilerplate!(InspectorPanel);

impl BasePanel for InspectorPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for InspectorPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-inspector")
    }
}

impl Render for InspectorPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Inspection {
            title,
            subtitle,
            value,
            fields,
        } = inspector::current(cx);
        let root = v_flex()
            .id("inspector")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .text_sm();
        if title.is_empty() && value.is_none() && fields.is_empty() {
            return root
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(t("inspector-empty"));
        }
        let copy = value.clone();
        root.overflow_y_scroll()
            .p_2()
            .gap_2()
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        v_flex()
                            .flex_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .when_some(copy, |d, v| {
                        d.child(
                            Button::new("inspector-copy")
                                .ghost()
                                .xsmall()
                                .icon(icons::Lucide::Copy)
                                .tooltip(t("inspector-copy"))
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(v.clone()))
                                }),
                        )
                    }),
            )
            .when_some(value, |d, v| {
                d.child(
                    div()
                        .id("inspector-value")
                        .test_support()
                        .p_2()
                        .rounded_md()
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .max_h(px(260.))
                        .overflow_y_scroll()
                        .child(v),
                )
            })
            .when(!fields.is_empty(), |d| {
                d.child(
                    v_flex()
                        .gap_0p5()
                        .children(fields.into_iter().map(|(k, v)| {
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(110.))
                                        .flex_shrink_0()
                                        .text_color(theme.muted_foreground)
                                        .child(k),
                                )
                                .child(div().flex_1().overflow_hidden().child(v))
                        })),
                )
            })
    }
}
