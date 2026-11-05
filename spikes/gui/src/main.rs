//! Phase 0 GUI spike (plan, section 7): GPUI Kit under the loads FerroDb needs.
//!
//! - Grid: `DataTable` with 1,000,000 rows × 50 columns (Arrow batches generated on demand)
//! - Tree: 100,000 documents + one 5 MB document
//! - Keys: 1,000,000 Redis-like keys in a `uniform_list`
//! - Editor: 50,000-line SQL file with tree-sitter highlighting
//! - Dock: three panels, layout saved to and restored from JSON
//!
//! `cargo xtask spike gui` — interactive.
//! `cargo xtask spike gui --bench [--out report.txt]` — auto-scrolls each view
//! for a few seconds, prints FPS and worst frame time, then exits.
//! The go/no-go threshold is ≥ 60 fps on every system (plan, section 7).

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use arrow_array::{
    Array, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray,
    TimestampMillisecondArray,
};
use arrow_schema::{DataType, Schema};
use ferrodb_mock::data;
use gpui_kit::{
    component::{
        ActiveTheme,
        dock::{
            BasePanel, DockArea, DockAreaState, DockEvent, DockLayout, DockSkin, Panel, PanelEvent,
            PanelStyle, panel_handle, register_panel,
        },
        h_flex,
        input::{Editor, EditorState},
        list::ListItem,
        tab::{Tab, TabBar},
        table::{Column, DataTable, TableDelegate, TableState},
        tree::{Tree, TreeItem, TreeState},
        v_flex,
    },
    prelude::*,
    *,
};

const ROWS: usize = 1_000_000;
const COLS: usize = 50;
const DOCS: usize = 100_000;
const KEYS: usize = 1_000_000;
const EDITOR_LINES: usize = 50_000;
const PAGE: usize = 1_000;
const BENCH_PER_VIEW: Duration = Duration::from_secs(4);
/// Benchmark cases: (label, view index).
const BENCH_CASES: [(&str, usize); 4] = [
    ("Grid scroll", 0),
    ("Grid random jump", 0),
    ("Tree 100k docs", 1),
    ("Keys 1M", 2),
];
const VIEWS: [&str; 5] = [
    "Grid 1M×50",
    "Tree 100k docs",
    "Keys 1M",
    "Editor 50k lines",
    "Dock",
];

// ------------------------------------------------------------------ grid

struct GridDelegate {
    schema: Arc<Schema>,
    columns: Vec<Column>,
    pages: HashMap<usize, RecordBatch>,
    generated_pages: usize,
}

impl GridDelegate {
    fn new() -> Self {
        let schema = data::table_schema(COLS);
        let columns = schema
            .fields()
            .iter()
            .map(|f| Column::new(f.name().clone(), f.name().clone()).width(px(130.)))
            .collect();
        Self {
            schema,
            columns,
            pages: HashMap::new(),
            generated_pages: 0,
        }
    }

    fn page(&mut self, row: usize) -> &RecordBatch {
        let p = row / PAGE;
        if !self.pages.contains_key(&p) {
            if self.pages.len() > 64 {
                self.pages.clear();
            }
            self.generated_pages += 1;
            let batch = data::table_batch(&self.schema, (p * PAGE) as u64, PAGE);
            self.pages.insert(p, batch);
        }
        &self.pages[&p]
    }

    fn cell(&mut self, row: usize, col: usize) -> SharedString {
        let batch = self.page(row);
        let a = batch.column(col);
        let i = row % PAGE;
        if a.is_null(i) {
            return "NULL".into();
        }
        match a.data_type() {
            DataType::Int64 => a
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .value(i)
                .to_string(),
            DataType::Utf8 => a
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(i)
                .to_owned(),
            DataType::Float64 => format!(
                "{:.2}",
                a.as_any().downcast_ref::<Float64Array>().unwrap().value(i)
            ),
            DataType::Boolean => a
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap()
                .value(i)
                .to_string(),
            DataType::Timestamp(..) => a
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .unwrap()
                .value(i)
                .to_string(),
            other => format!("{other}"),
        }
        .into()
    }
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        COLS
    }
    fn rows_count(&self, _: &App) -> usize {
        ROWS
    }
    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }
    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        self.cell(row_ix, col_ix)
    }
}

// ------------------------------------------------------------------ dock panels

struct NotePanel {
    name: &'static str,
    focus: FocusHandle,
}

impl EventEmitter<PanelEvent> for NotePanel {}
impl Focusable for NotePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl BasePanel for NotePanel {
    fn panel_name(&self) -> &'static str {
        self.name
    }
}
impl Panel for NotePanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.name
    }
}
impl Render for NotePanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_2().child(format!(
            "Panel {} — drag tabs, resize, restart: layout persists",
            self.name
        ))
    }
}

fn note(name: &'static str, cx: &mut App) -> Entity<NotePanel> {
    cx.new(|cx| NotePanel {
        name,
        focus: cx.focus_handle(),
    })
}

fn dock_layout_path() -> PathBuf {
    std::env::temp_dir().join("ferrodb-spike-dock-layout.json")
}

// ------------------------------------------------------------------ spike view

struct BenchState {
    view: usize,
    view_started: Instant,
    last_frame: Instant,
    frames: u32,
    worst: Duration,
    pos: usize,
    results: Vec<String>,
    out: Option<PathBuf>,
}

struct Spike {
    active: usize,
    grid: Entity<TableState<GridDelegate>>,
    tree: Entity<TreeState>,
    keys_scroll: UniformListScrollHandle,
    editor: Entity<EditorState>,
    dock: Entity<DockArea>,
    timings: Vec<String>,
    bench: Option<BenchState>,
    _sub: Subscription,
}

fn editor_text() -> String {
    let templates = [
        "SELECT c.id, c.name, sum(o.total) AS revenue FROM customers c JOIN orders o ON o.customer_id = c.id",
        "WHERE o.created_at >= DATE '2026-01-01' AND c.city IN ('Warszawa', 'Kraków')",
        "GROUP BY c.id, c.name HAVING sum(o.total) > 1000 ORDER BY revenue DESC LIMIT 100;",
        "-- comment line with some text to make lines longer and more realistic for the editor",
        "INSERT INTO audit_log (event, payload) VALUES ('login', '{\"user\": 42, \"ok\": true}');",
        "UPDATE products SET price = price * 1.05 WHERE category_id = 7 AND active;",
    ];
    let mut s = String::with_capacity(EDITOR_LINES * 90);
    for i in 0..EDITOR_LINES {
        s.push_str(templates[i % templates.len()]);
        s.push('\n');
    }
    s
}

fn tree_items() -> Vec<TreeItem> {
    let mut items = Vec::with_capacity(DOCS + 1);
    let big = data::big_document(5 * 1024 * 1024);
    let big_children: Vec<TreeItem> = big
        .get_array("items")
        .ok()
        .map(|arr| {
            arr.into_iter()
                .enumerate()
                .map(|(i, _)| {
                    TreeItem::new(
                        format!("big/{i}"),
                        format!("items[{i}]: {{ i: {i}, v: … }}"),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    items.push(
        TreeItem::new(
            "big",
            format!("big-document (5 MB, {} items)", big_children.len()),
        )
        .children(big_children),
    );
    for i in 0..DOCS {
        let doc = data::document(i as u64);
        let name = doc.get_str("name").unwrap_or_default().to_owned();
        let id = doc.get_str("_id").unwrap_or_default().to_owned();
        let age = doc.get_i32("age").unwrap_or_default();
        items.push(
            TreeItem::new(
                format!("d{i}"),
                format!("{{ _id: \"{id}\", name: \"{name}\" }}"),
            )
            .children([
                TreeItem::new(format!("d{i}/name"), format!("name: \"{name}\"")),
                TreeItem::new(format!("d{i}/age"), format!("age: {age}")),
                TreeItem::new(format!("d{i}/address"), "address: { … }"),
            ]),
        );
    }
    items
}

impl Spike {
    fn new(bench: Option<Option<PathBuf>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut timings = Vec::new();
        let t = Instant::now();
        let grid = cx.new(|cx| TableState::new(GridDelegate::new(), window, cx));
        timings.push(format!("grid state created in {:?}", t.elapsed()));

        let t = Instant::now();
        let items = tree_items();
        timings.push(format!(
            "tree items ({} docs + 5 MB doc) built in {:?}",
            DOCS,
            t.elapsed()
        ));
        let tree = cx.new(|cx| TreeState::new(cx).items(items));

        let t = Instant::now();
        let text = editor_text();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .default_value(text)
        });
        timings.push(format!(
            "editor with {EDITOR_LINES} lines loaded in {:?}",
            t.elapsed()
        ));

        register_panel(cx, "A", |_, _, cx| panel_handle(note("A", cx)));
        register_panel(cx, "B", |_, _, cx| panel_handle(note("B", cx)));
        register_panel(cx, "C", |_, _, cx| panel_handle(note("C", cx)));
        let (dock, skin) = DockSkin::dock_area("spike-dock", Some(1), window, cx);
        skin.set_panel_style(PanelStyle::TabBar, cx);
        let restored = std::fs::read_to_string(dock_layout_path())
            .ok()
            .and_then(|s| serde_json::from_str::<DockAreaState>(&s).ok())
            .is_some_and(|state| dock.update(cx, |d, cx| d.load(state, window, cx)).is_ok());
        if !restored {
            let (a, b, c) = (note("A", cx), note("B", cx), note("C", cx));
            dock.update(cx, |d, cx| {
                d.set_center(
                    DockLayout::h_split()
                        .child(
                            DockLayout::tabs()
                                .panel_view(panel_handle(a), cx)
                                .panel_view(panel_handle(b), cx),
                            None,
                        )
                        .child(DockLayout::tabs().panel_view(panel_handle(c), cx), None),
                    window,
                    cx,
                )
            });
        }
        timings.push(format!("dock layout restored from disk: {restored}"));
        let sub = cx.subscribe(&dock, |_, dock, ev: &DockEvent, cx| {
            if matches!(ev, DockEvent::LayoutChanged) {
                let state = dock.read(cx).dump(cx);
                if let Ok(json) = serde_json::to_string_pretty(&state) {
                    let _ = std::fs::write(dock_layout_path(), json);
                }
            }
        });

        for t in &timings {
            println!("[spike] {t}");
        }
        let now = Instant::now();
        Self {
            active: 0,
            grid,
            tree,
            keys_scroll: UniformListScrollHandle::new(),
            editor,
            dock,
            timings,
            bench: bench.map(|out| BenchState {
                view: 0,
                view_started: now,
                last_frame: now,
                frames: 0,
                worst: Duration::ZERO,
                pos: 0,
                results: Vec::new(),
                out,
            }),
            _sub: sub,
        }
    }

    /// One benchmark step per frame: scroll the active view and record timing.
    fn bench_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(b) = self.bench.as_mut() else { return };
        let now = Instant::now();
        if b.frames > 0 {
            b.worst = b.worst.max(now - b.last_frame);
        }
        b.last_frame = now;
        b.frames += 1;
        b.pos += 37;
        match BENCH_CASES[b.view].1 {
            // Continuous scrolling: 20 rows per frame (~1200 rows/s), new data page every 50 frames.
            0 if b.view == 0 => {
                let row = (b.pos / 37) * 20 % ROWS;
                self.grid.update(cx, |t, cx| t.scroll_to_row(row, cx));
            }
            // Worst case: random jump + new column every frame, fresh 1000×50 page each time.
            0 => {
                let (row, col) = (b.pos * 29 % ROWS, (b.pos / 37) % COLS);
                self.grid.update(cx, |t, cx| {
                    t.scroll_to_row(row, cx);
                    t.scroll_to_col(col, cx);
                });
            }
            1 => self.tree.update(cx, |t, _| {
                t.scroll_to_item(b.pos * 3 % DOCS, ScrollStrategy::Top)
            }),
            2 => self
                .keys_scroll
                .scroll_to_item(b.pos * 29 % KEYS, ScrollStrategy::Top),
            _ => {}
        }
        let elapsed = now - b.view_started;
        if elapsed >= BENCH_PER_VIEW {
            let fps = b.frames as f64 / elapsed.as_secs_f64();
            let line = format!(
                "{:<18} {:>6.1} fps  worst frame {:>6.1} ms  ({} frames)",
                BENCH_CASES[b.view].0,
                fps,
                b.worst.as_secs_f64() * 1000.,
                b.frames
            );
            println!("[bench] {line}");
            b.results.push(line);
            b.view += 1;
            b.frames = 0;
            b.worst = Duration::ZERO;
            b.pos = 0;
            b.view_started = Instant::now();
            if b.view >= BENCH_CASES.len() {
                let mut report = self.timings.clone();
                report.extend(b.results.iter().cloned());
                let report = report.join("\n");
                if let Some(out) = &b.out {
                    let _ = std::fs::write(out, &report);
                }
                cx.quit();
                return;
            }
            self.active = BENCH_CASES[b.view].1;
        }
        window.request_animation_frame();
        cx.notify();
    }

    fn render_keys(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let muted = cx.theme().muted_foreground;
        uniform_list("keys", KEYS, move |range, _, _| {
            range
                .map(|i| {
                    let k = data::key(i as u64);
                    h_flex()
                        .id(i)
                        .h(px(22.))
                        .px_2()
                        .gap_4()
                        .child(div().w(px(380.)).child(k.key))
                        .child(
                            div()
                                .w(px(80.))
                                .text_color(muted)
                                .child(format!("{:?}", k.kind)),
                        )
                        .child(
                            div().w(px(100.)).text_color(muted).child(
                                k.ttl
                                    .map(|t| format!("{}s", t.as_secs()))
                                    .unwrap_or_else(|| "∞".into()),
                            ),
                        )
                        .child(
                            div()
                                .text_color(muted)
                                .child(format!("{} B", k.size.unwrap_or(0))),
                        )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.keys_scroll)
        .size_full()
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.bench_frame(window, cx);
        let body = match self.active {
            0 => DataTable::new(&self.grid).stripe(true).into_any_element(),
            1 => Tree::new(&self.tree, |ix, entry, selected, _, _| {
                ListItem::new(ix)
                    .selected(selected)
                    .pl(px(8. + 16. * entry.depth() as f32))
                    .child(entry.item().label.clone())
            })
            .size_full()
            .into_any_element(),
            2 => self.render_keys(cx).into_any_element(),
            3 => Editor::new(&self.editor).size_full().into_any_element(),
            _ => self.dock.clone().into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TabBar::new("views")
                    .selected_index(self.active)
                    .children(VIEWS.iter().map(|v| Tab::new().label(*v)))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.active = *ix;
                        cx.notify();
                    })),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bench = args.iter().any(|a| a == "--bench").then(|| {
        args.iter()
            .position(|a| a == "--out")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
    });
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            let bounds = Bounds::centered(None, size(px(1400.), px(900.)), cx);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                cx,
                move |window, cx| {
                    window.set_window_title("FerroDb — GUI spike");
                    cx.new(|cx| Spike::new(bench, window, cx))
                },
            )
            .expect("open window");
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            cx.activate(true);
        });
}
