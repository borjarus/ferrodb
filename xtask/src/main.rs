//! Developer automation: `cargo xtask <command>`.
//!
//! Commands:
//! - `ci`       — the same checks CI runs locally (fmt, clippy, tests)
//! - `fmt`      — format the workspace
//! - `lint`     — clippy with `-D warnings`
//! - `test`     — workspace tests (nextest when installed, otherwise `cargo test`)
//! - `package`  — build the release binary, package it with `cargo-packager`
//!   and sign what the environment allows (see `release.rs`)
//! - `release-keygen [--out F]` — a new Ed25519 update signing key
//! - `release-manifest --version V --channel C --notes N --dir D --base-url U`
//!   — the signed update manifest of the packages in `D`
//! - `spike <name> [args]` — run a phase 0 spike (`gui`, `adbc`, `jdbc-reader`)
//! - `db-up` / `db-down` — start / remove the TCK database containers
//! - `tck`      — run the driver TCK against those containers
//! - `env`      — print the TCK variables (`KEY=value`, for `$GITHUB_ENV`)
//!
//!   These four take `--only key,key` or `--group native|nosql|jdbc|all`;
//!   `db-up` also `--image key=image` and `--no-wait`, `tck` `--skip-meta`.
//! - `matrix`   — print the integration test matrix (`--check` / `--write`
//!   `docs/test-matrix.md`, `--gha` for the nightly workflow)

mod db;
mod matrix;
mod release;

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
        "release-keygen" => release::keygen(rest),
        "release-manifest" => release::manifest_cmd(rest),
        "spike" => spike(rest),
        "db-up" => db::Selection::parse(rest).and_then(|s| db::up(&s)),
        "db-down" => db::Selection::parse(rest).and_then(|s| db::down(&s)),
        "tck" => db::Selection::parse(rest).and_then(|s| db::tck(&s)),
        "env" => db::Selection::parse(rest).and_then(|s| db::print_env(&s)),
        "matrix" => matrix::run(rest),
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
    eprintln!(
        "usage: cargo xtask <ci|fmt|lint|test|package|release-keygen|release-manifest|matrix|spike <gui|adbc|jdbc-reader> [args]>
                cargo xtask <db-up|db-down|tck|env> [--only key,key | --group native|nosql|jdbc|all]
                            [--image key=image] [--no-wait] [--skip-meta]"
    );
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
    let out = root().join("target").join("packages");
    let packager = |formats: Option<&str>| {
        let mut cmd = Command::new("cargo-packager");
        cmd.args(["--release", "-p", "ferrodb-app", "--out-dir"])
            .arg(&out);
        if let Some(f) = formats {
            cmd.args(["--formats", f]);
        }
        run(&mut cmd)
    };
    if cfg!(windows) {
        // The installed executable, then the installer.
        let exe = root().join("target").join("release").join("ferrodb.exe");
        release::sign_windows(std::slice::from_ref(&exe))?;
        packager(None)?;
        release::sign_windows(&release::files_with(&out, &["-setup.exe", ".msi"]))
    } else if cfg!(target_os = "macos") {
        let Ok(identity) = env::var("FERRODB_MACOS_SIGN_IDENTITY") else {
            eprintln!(
                "xtask: warning: macOS signing skipped (FERRODB_MACOS_SIGN_IDENTITY not set)"
            );
            return packager(None);
        };
        // Sign the bundle, then build, sign, notarize and staple the disk image.
        packager(Some("app"))?;
        let app = out.join("FerroDb.app");
        release::codesign_macos(&identity, &app)?;
        let arch = if env::consts::ARCH == "aarch64" {
            "aarch64"
        } else {
            "x64"
        };
        let dmg = out.join(format!("FerroDb_{}_{arch}.dmg", env!("CARGO_PKG_VERSION")));
        let _ = std::fs::remove_file(&dmg);
        run(Command::new("hdiutil")
            .args([
                "create",
                "-volname",
                "FerroDb",
                "-format",
                "UDZO",
                "-ov",
                "-srcfolder",
            ])
            .arg(&app)
            .arg(&dmg))?;
        release::codesign_macos(&identity, &dmg)?;
        release::notarize_macos(&dmg)
    } else {
        packager(None)?;
        release::gpg_sign(&release::files_with(&out, &[".appimage", ".deb"]))
    }
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
