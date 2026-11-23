//! Phase 10.2: fuzzing and regression benchmarks.
//!
//! - `fuzz <target|all> [seconds]` — cargo-fuzz (nightly + libFuzzer) on a
//!   target of `fuzz/`; new inputs go to `target/fuzz-corpus/<target>`,
//!   the checked-in seeds in `fuzz/corpus/<target>` are only read;
//! - `fuzz-smoke [cases]` — the same harness as stable proptest tests;
//! - `bench [filter]` — the divan benchmarks of every crate that has them;
//! - `bench-check` — the benchmarks against `benches/budgets.toml`: fails
//!   when a median exceeds its budget × tolerance.

use std::{
    collections::BTreeMap,
    env,
    process::{Command, Stdio},
};

use crate::{cargo, root, run};

/// Crates with fuzz smoke tests (`tests/fuzz_smoke.rs`).
const SMOKE_CRATES: &[&str] = &["ferrodb-lang", "ferrodb-data", "ferrodb-jdbc"];

/// (package, bench target) of every divan benchmark.
const BENCHES: &[(&str, &str)] = &[
    ("ferrodb-lang", "lang"),
    ("ferrodb-data", "data"),
    ("ferrodb-jdbc", "jdbc"),
];

fn fuzz_targets() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root().join("fuzz/fuzz_targets"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".rs"))
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn fuzz(rest: &[String]) -> Result<(), String> {
    let targets = fuzz_targets();
    let Some(target) = rest.first() else {
        return Err(format!(
            "fuzz target required: all | {}",
            targets.join(" | ")
        ));
    };
    let secs: u64 = match rest.get(1) {
        Some(s) => s
            .parse()
            .map_err(|_| format!("bad number of seconds `{s}`"))?,
        None => 60,
    };
    let nightly = Command::new("cargo")
        .args(["+nightly", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    let cargo_fuzz = Command::new("cargo")
        .args(["fuzz", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !nightly || !cargo_fuzz {
        return Err(format!(
            "cargo-fuzz needs a nightly toolchain and libFuzzer:\n\
             \x20 rustup toolchain install nightly\n\
             \x20 cargo install cargo-fuzz\n\
             (missing: {}{}). libFuzzer works on Linux and macOS; on Windows it is \
             experimental (MSVC AddressSanitizer). The same harness runs on stable \
             Rust with `cargo xtask fuzz-smoke`.",
            if nightly { "" } else { "nightly toolchain " },
            if cargo_fuzz { "" } else { "cargo-fuzz" },
        ));
    }
    // The fuzz project has its own workspace: start from the main lock file
    // so it builds the same dependency versions.
    let lock = root().join("fuzz/Cargo.lock");
    if !lock.exists() {
        std::fs::copy(root().join("Cargo.lock"), &lock)
            .map_err(|e| format!("copying Cargo.lock: {e}"))?;
    }
    let selected: Vec<String> = if target == "all" {
        targets
    } else if targets.contains(target) {
        vec![target.clone()]
    } else {
        return Err(format!(
            "unknown fuzz target `{target}` ({})",
            targets.join(", ")
        ));
    };
    for t in selected {
        let work = root().join("target/fuzz-corpus").join(&t);
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        run(Command::new("cargo")
            .args(["+nightly", "fuzz", "run", &t])
            .arg(&work)
            .arg(root().join("fuzz/corpus").join(&t))
            .args(["--", &format!("-max_total_time={secs}"), "-timeout=10"])
            .current_dir(root().join("fuzz")))?;
    }
    Ok(())
}

pub fn fuzz_smoke(rest: &[String]) -> Result<(), String> {
    let cases = rest.first().map_or("2000", String::as_str);
    let mut args = vec!["test"];
    for c in SMOKE_CRATES {
        args.extend(["-p", c]);
    }
    args.extend(["--test", "fuzz_smoke"]);
    eprintln!("xtask: PROPTEST_CASES={cases}");
    run(
        Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(&args)
            .env("PROPTEST_CASES", cases),
    )
}

pub fn bench(rest: &[String]) -> Result<(), String> {
    for (pkg, name) in BENCHES {
        let mut args = vec!["bench", "-p", pkg, "--bench", name, "--"];
        args.extend(rest.iter().map(String::as_str));
        cargo(&args)?;
    }
    Ok(())
}

pub fn bench_check(rest: &[String]) -> Result<(), String> {
    let path = root().join("benches/budgets.toml");
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let budgets = Budgets::parse(&text)?;
    let tolerance = env::var("FERRODB_BENCH_TOLERANCE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(budgets.tolerance);
    let mut measured: BTreeMap<String, f64> = BTreeMap::new();
    for (pkg, name) in BENCHES {
        let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        cmd.args([
            "bench", "-p", pkg, "--bench", name, "--", "--color", "never",
        ])
        .args(rest)
        .current_dir(root())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
        eprintln!("$ {cmd:?}");
        let out = cmd
            .output()
            .map_err(|e| format!("failed to start cargo bench: {e}"))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        print!("{stdout}");
        if !out.status.success() {
            return Err(format!("cargo bench -p {pkg} --bench {name} failed"));
        }
        for (bench, ns) in parse_divan(&stdout) {
            measured.insert(format!("{pkg}/{name}/{bench}"), ns);
        }
    }
    let filtered = !rest.is_empty();
    let mut failures = Vec::new();
    println!("\nbudget check (tolerance × {tolerance}):");
    for (key, budget) in &budgets.entries {
        match measured.get(key) {
            Some(&ns) => {
                let limit = budget * tolerance;
                let ok = ns <= limit;
                println!(
                    "  {} {key}: median {} / budget {}",
                    if ok { "ok  " } else { "FAIL" },
                    show(ns),
                    show(*budget)
                );
                if !ok {
                    failures.push(key.clone());
                }
            }
            None if filtered => {}
            None => {
                println!("  FAIL {key}: not measured (renamed or removed benchmark?)");
                failures.push(key.clone());
            }
        }
    }
    for key in measured
        .keys()
        .filter(|k| !budgets.entries.contains_key(*k))
    {
        println!("  note {key}: no budget");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} benchmark(s) over budget: {}",
            failures.len(),
            failures.join(", ")
        ))
    }
}

/// `benches/budgets.toml`: `tolerance = 1.5` and a `[budgets]` table of
/// `"<package>/<bench>/<path>" = "<number> <ns|us|µs|ms|s>"`.
struct Budgets {
    tolerance: f64,
    entries: BTreeMap<String, f64>,
}

impl Budgets {
    fn parse(text: &str) -> Result<Self, String> {
        let v: toml::Table = text.parse().map_err(|e| format!("budgets.toml: {e}"))?;
        let tolerance = match v.get("tolerance") {
            Some(t) => t
                .as_float()
                .or_else(|| t.as_integer().map(|i| i as f64))
                .ok_or("budgets.toml: `tolerance` must be a number")?,
            None => 1.0,
        };
        let mut entries = BTreeMap::new();
        if let Some(t) = v.get("budgets").and_then(toml::Value::as_table) {
            for (k, val) in t {
                let s = val
                    .as_str()
                    .ok_or_else(|| format!("budgets.toml: `{k}` must be a string like \"5 ms\""))?;
                let ns = parse_duration(s)
                    .ok_or_else(|| format!("budgets.toml: `{k}`: bad duration `{s}`"))?;
                entries.insert(k.clone(), ns);
            }
        }
        Ok(Self { tolerance, entries })
    }
}

/// `1.234 ms` → nanoseconds.
fn parse_duration(s: &str) -> Option<f64> {
    let mut it = s.split_whitespace();
    let n: f64 = it.next()?.parse().ok()?;
    let unit = match it.next()? {
        "ps" => 1e-3,
        "ns" => 1.0,
        "us" | "µs" | "μs" => 1e3,
        "ms" => 1e6,
        "s" => 1e9,
        _ => return None,
    };
    it.next().is_none().then_some(n * unit)
}

fn show(ns: f64) -> String {
    if ns >= 1e9 {
        format!("{:.3} s", ns / 1e9)
    } else if ns >= 1e6 {
        format!("{:.3} ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.3} µs", ns / 1e3)
    } else {
        format!("{ns:.1} ns")
    }
}

/// Medians from divan's tree output:
///
/// ```text
/// lang                     fastest       │ slowest       │ median        │ mean   …
/// ├─ split                               │               │               │
/// │  ├─ mql_50k_lines      1.1 ms        │ 1.9 ms        │ 1.2 ms        │ 1.2 ms …
/// ```
///
/// Each tree level is three columns wide; the path of a row is the names
/// of its ancestors below the root (the bench binary). Rows whose third
/// cell is a duration are measurements.
fn parse_divan(out: &str) -> Vec<(String, f64)> {
    let mut stack: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for line in out.lines() {
        if !line.contains('│') {
            continue;
        }
        let prefix = line
            .chars()
            .take_while(|c| matches!(c, '│' | '├' | '╰' | '─' | ' '))
            .count();
        let rest: String = line.chars().skip(prefix).collect();
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        let depth = prefix / 3;
        stack.truncate(depth);
        stack.push(name.to_owned());
        let cells: Vec<&str> = rest.split('│').map(str::trim).collect();
        if depth == 0 {
            continue;
        }
        if let Some(median) = cells.get(2).and_then(|c| parse_duration(c)) {
            rows.push((stack[1..].join("/"), median));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_divan_tree() {
        let out = "\
Timer precision: 100 ns
lang                      fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ diagnostics_1k_lines   2.1 ms        │ 3 ms          │ 2.2 ms        │ 2.3 ms        │ 100     │ 100
├─ complete                             │               │               │               │         │
│  ├─ columns_10k         10.5 µs       │ 20 µs         │ 11.25 µs      │ 12 µs         │ 100     │ 400
│  ╰─ tables_10k          1.5 ms        │ 2 ms          │ 1.6 ms        │ 1.6 ms        │ 100     │ 100
╰─ split                                │               │               │               │         │
   ╰─ mql_50k_lines       900 ns        │ 1 µs          │ 950 ns        │ 960 ns        │ 100     │ 800
                          1.1 GB/s      │ 1 GB/s        │ 1.05 GB/s     │ 1 GB/s        │         │
";
        let rows = parse_divan(out);
        assert_eq!(
            rows,
            vec![
                ("diagnostics_1k_lines".to_owned(), 2.2e6),
                ("complete/columns_10k".to_owned(), 11_250.0),
                ("complete/tables_10k".to_owned(), 1.6e6),
                ("split/mql_50k_lines".to_owned(), 950.0),
            ]
        );
    }

    #[test]
    fn budgets_file() {
        let b =
            Budgets::parse("tolerance = 2\n[budgets]\n\"ferrodb-lang/lang/split/x\" = \"40 ms\"\n")
                .unwrap();
        assert_eq!(b.tolerance, 2.0);
        assert_eq!(b.entries["ferrodb-lang/lang/split/x"], 4e7);
        assert!(Budgets::parse("[budgets]\nx = \"4 parsecs\"").is_err());
    }
}
