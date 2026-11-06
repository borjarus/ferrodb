//! Dock panels. Every panel is registered by name so that a saved layout can
//! be rebuilt (`DockArea::load`).

pub mod console;
pub mod explorer;
pub mod gallery;
pub mod history;
pub mod inspector;
pub mod local_history;
pub mod output_panel;
pub mod services;
pub mod welcome;

use ferrodb_ui_kit::{
    gpui::{App, AppContext as _},
    kit::dock::{panel_handle, register_panel},
};

pub use console::ConsolePanel;
pub use explorer::ExplorerPanel;
pub use gallery::GalleryPanel;
pub use history::HistoryPanel;
pub use inspector::InspectorPanel;
pub use local_history::LocalHistoryPanel;
pub use output_panel::OutputPanel;
pub use services::ServicesPanel;
pub use welcome::WelcomePanel;

/// Implements the boilerplate traits every panel needs.
macro_rules! panel_boilerplate {
    ($ty:ty) => {
        impl ::ferrodb_ui_kit::gpui::EventEmitter<::ferrodb_ui_kit::kit::dock::PanelEvent> for $ty {}
        impl ::ferrodb_ui_kit::gpui::Focusable for $ty {
            fn focus_handle(
                &self,
                _: &::ferrodb_ui_kit::gpui::App,
            ) -> ::ferrodb_ui_kit::gpui::FocusHandle {
                self.focus.clone()
            }
        }
    };
}
pub(crate) use panel_boilerplate;

/// Registers all panels in the GPUI Kit panel registry.
pub fn register(cx: &mut App) {
    register_panel(cx, explorer::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| ExplorerPanel::new(window, cx)))
    });
    register_panel(cx, console::NAME, |ctx, window, cx| {
        let state = ctx.state().clone();
        panel_handle(cx.new(|cx| ConsolePanel::restore(&state, window, cx)))
    });
    register_panel(cx, welcome::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| WelcomePanel::new(window, cx)))
    });
    register_panel(cx, services::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| ServicesPanel::new(window, cx)))
    });
    register_panel(cx, output_panel::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| OutputPanel::new(window, cx)))
    });
    register_panel(cx, inspector::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| InspectorPanel::new(window, cx)))
    });
    register_panel(cx, history::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| HistoryPanel::new(window, cx)))
    });
    register_panel(cx, local_history::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| LocalHistoryPanel::new(window, cx)))
    });
    register_panel(cx, gallery::NAME, |_, window, cx| {
        panel_handle(cx.new(|cx| GalleryPanel::new(window, cx)))
    });
}
