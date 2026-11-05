use std::time::UNIX_EPOCH;

use ferrodb_ui_kit::{
    gpui::{prelude::*, *},
    kit::{
        ActiveTheme,
        dock::{BasePanel, Panel},
        h_flex,
    },
};

use super::panel_boilerplate;
use crate::{
    i18n::t,
    output::{self, Level, OutputLog},
};

pub const NAME: &str = "Output";

pub struct OutputPanel {
    focus: FocusHandle,
    log: Entity<OutputLog>,
    scroll: UniformListScrollHandle,
    _observe: Subscription,
}

impl OutputPanel {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let log = output::entity(cx);
        let scroll = UniformListScrollHandle::new();
        let observe = cx.observe(&log, |this: &mut Self, log, cx| {
            let n = log.read(cx).lines.len();
            if n > 0 {
                this.scroll.scroll_to_item(n - 1, ScrollStrategy::Bottom);
            }
            cx.notify();
        });
        Self {
            focus: cx.focus_handle(),
            log,
            scroll,
            _observe: observe,
        }
    }
}

panel_boilerplate!(OutputPanel);

impl BasePanel for OutputPanel {
    fn panel_name(&self) -> &'static str {
        NAME
    }
}

impl Panel for OutputPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t("panel-output")
    }
}

fn clock(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

impl Render for OutputPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.log.read(cx).lines.len();
        div()
            .id("output")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .child(
                uniform_list(
                    "output-lines",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        let theme = cx.theme();
                        let (muted, warn, err, fg) = (
                            theme.muted_foreground,
                            theme.warning,
                            theme.danger,
                            theme.foreground,
                        );
                        let log = this.log.read(cx);
                        range
                            .filter_map(|ix| log.lines.get(ix).map(|l| (ix, l.clone())))
                            .map(|(ix, line)| {
                                let color = match line.level {
                                    Level::Info => fg,
                                    Level::Warning => warn,
                                    Level::Error => err,
                                };
                                h_flex()
                                    .id(ix)
                                    .gap_2()
                                    .px_2()
                                    .child(div().text_color(muted).child(clock(line.at)))
                                    .child(div().text_color(color).child(line.text))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            )
    }
}
