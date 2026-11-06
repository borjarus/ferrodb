//! Component gallery (plan 2.9): `cargo run -p ferrodb-gallery [SECTION]`
//! (SECTION = index into `ferrodb_ui::panels::gallery::SECTIONS`, default 0).
//!
//! Shows every phase-2 component on mock data, switches between the light
//! and dark theme and between mock scenarios. Nothing is written to disk.

use std::sync::Arc;

use ferrodb_mock::{MockBackend, Scenario};
use ferrodb_ui::{AppState, ScenarioControl, panels::GalleryPanel};
use ferrodb_ui_kit::gpui::{App, AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};

struct MockScenarios(MockBackend);

impl ScenarioControl for MockScenarios {
    fn scenarios(&self) -> Vec<&'static str> {
        Scenario::ALL.iter().map(|s| s.id()).collect()
    }
    fn current(&self) -> &'static str {
        self.0.scenario().id()
    }
    fn set(&self, id: &str) {
        if let Some(s) = Scenario::from_id(id) {
            self.0.set_scenario(s);
        }
    }
}

fn main() {
    let section: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(0);
    let mock = MockBackend::new();
    let scenarios: Arc<dyn ScenarioControl> = Arc::new(MockScenarios(mock.clone()));
    ferrodb_ui_kit::application().run(move |cx: &mut App| {
        ferrodb_ui::init(
            AppState {
                backend: Arc::new(mock),
                scenarios: Some(scenarios),
                paths: None,
            },
            cx,
        );
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(1400.), px(900.)),
                cx,
            ))),
            ..Default::default()
        };
        if let Err(e) = ferrodb_ui_kit::open_window(options, cx, |window, cx| {
            window.set_window_title("FerroDb — component gallery");
            cx.new(|cx| {
                let mut g = GalleryPanel::new(window, cx);
                g.active = section.min(ferrodb_ui::panels::gallery::SECTIONS.len() - 1);
                g
            })
        }) {
            eprintln!("cannot open the gallery window: {e}");
            cx.quit();
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
