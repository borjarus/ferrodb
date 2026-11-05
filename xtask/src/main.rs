//! Developer automation: `cargo xtask <command>`.
//!
//! Commands:
//! - `ci`       — the same checks CI runs locally (fmt, clippy, tests)
//! - `fmt`      — format the workspace
//! - `lint`     — clippy with `-D warnings`
//! - `test`     — workspace tests (nextest when installed, otherwise `cargo test`)
//! - `package`  — build the release binary and package it with `cargo-packager`
//! - `spike <name> [args]` — run a phase 0 spike (`gui`, `adbc`, `jdbc-reader`)

use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        print_help();
        return ExitCode::FAILURE;
    };
    let rest = &args[1..];
    let result = match cmd.as_str() {
        "ci" => ci(),
        "fmt" => cargo(&["fmt", "--all"]),
        "lint" => lint(),
        "test" => test(),
        "package" => package(),
        "spike" => spike(rest),
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        other => Err(format!("unknown command `{other}`")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    eprintln!("usage: cargo xtask <ci|fmt|lint|test|package|spike <gui|adbc|jdbc-reader> [args]>");
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the workspace root")
        .to_path_buf()
}

fn run(cmd: &mut Command) -> Result<(), String> {
    eprintln!("$ {cmd:?}");
    let status = cmd
        .current_dir(root())
        .status()
        .map_err(|e| format!("failed to start {cmd:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?} exited with {status}"))
    }
}

fn cargo(args: &[&str]) -> Result<(), String> {
    run(Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into())).args(args))
}

fn has_tool(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn lint() -> Result<(), String> {
    cargo(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ])
}

fn test() -> Result<(), String> {
    let nextest = Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if nextest {
        cargo(&["nextest", "run", "--workspace"])
    } else {
        cargo(&["test", "--workspace"])
    }
}

fn ci() -> Result<(), String> {
    cargo(&["fmt", "--all", "--", "--check"])?;
    lint()?;
    test()?;
    if has_tool("cargo-deny") {
        cargo(&["deny", "check"])?;
    } else {
        eprintln!("xtask: cargo-deny not installed, skipping license/advisory check");
    }
    Ok(())
}

fn package() -> Result<(), String> {
    cargo(&["build", "--release", "-p", "ferrodb-app"])?;
    if !has_tool("cargo-packager") {
        return Err(
            "cargo-packager is not installed (`cargo install cargo-packager --locked`)".into(),
        );
    }
    run(Command::new("cargo-packager")
        .args(["--release", "-p", "ferrodb-app", "--out-dir"])
        .arg(root().join("target").join("packages")))
}

fn spike(rest: &[String]) -> Result<(), String> {
    let Some(name) = rest.first() else {
        return Err("spike name required: gui | adbc | jdbc-reader".into());
    };
    let pkg = match name.as_str() {
        "gui" => "spike-gui",
        "adbc" => "spike-adbc",
        "jdbc-reader" => "spike-jdbc-reader",
        other => return Err(format!("unknown spike `{other}`")),
    };
    let mut args = vec!["run", "--release", "-p", pkg, "--"];
    args.extend(rest[1..].iter().map(String::as_str));
    cargo(&args)
}
