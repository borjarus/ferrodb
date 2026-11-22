//! Where ADBC driver libraries come from (ADR-007: drivers are downloaded
//! on demand, never bundled). The Apache Arrow ADBC project publishes the
//! Snowflake and BigQuery drivers (Go, compiled to a C shared library) as
//! Python wheels on PyPI; a wheel is a zip archive with the library inside.
//! This module lists the wheels per platform, with the SHA-256 published by
//! PyPI (checked against a download of the Windows wheels), the path of the
//! library inside the archive and the license, and extracts the library
//! from a downloaded archive. The driver manager (`ferrodb-driver-manager`)
//! downloads and verifies; [`extract_library`] is reusable by it.
//!
//! The library inside a wheel is always named `lib…so`, even on Windows and
//! macOS (where it is a PE / Mach-O file). It is extracted under the name
//! the platform loader expects ([`library_file_name`]).

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

use ferrodb_api::{Error, Requirement, Result};

/// One downloadable library build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Artifact {
    /// FerroDb driver id (`snowflake`, `bigquery`).
    pub driver: &'static str,
    /// Library name without prefix / extension (`adbc_driver_snowflake`).
    pub library: &'static str,
    pub version: &'static str,
    /// Rust target names: `windows` / `linux` / `macos`, `x86_64` / `aarch64`.
    pub os: &'static str,
    pub arch: &'static str,
    pub url: &'static str,
    /// SHA-256 of the archive at `url` (hex).
    pub sha256: &'static str,
    /// Archive size in bytes.
    pub size: u64,
    /// Path of the library inside the archive.
    pub path_in_archive: &'static str,
    /// SPDX license of the library.
    pub license: &'static str,
}

#[cfg(test)]
const PYPI: &str = "https://files.pythonhosted.org/packages";

/// Every published build, newest release first (Apache Arrow ADBC 1.11.0,
/// the latest stable `adbc-driver-snowflake` / `adbc-driver-bigquery` on
/// PyPI when this list was made).
pub const ARTIFACTS: &[Artifact] = &[
    // ---------------------------------------------------------- Snowflake
    Artifact {
        driver: "snowflake",
        library: "adbc_driver_snowflake",
        version: "1.11.0",
        os: "windows",
        arch: "x86_64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/e9/ca/542793fc9a164f013e3248061e922ac5d5246ce93718a5c0cb44bbd2219e/",
            "adbc_driver_snowflake-1.11.0-py3-none-win_amd64.whl"
        ),
        sha256: "83b1a363a44ae4cb1e4956f9cc6c4e44dddd7c768a0f0a8b42d9cb8eb8906305",
        size: 27_264_722,
        path_in_archive: "adbc_driver_snowflake/libadbc_driver_snowflake.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "snowflake",
        library: "adbc_driver_snowflake",
        version: "1.11.0",
        os: "linux",
        arch: "x86_64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/6f/26/baa44b8b9faaf26846dd8c0e513c7ce6862d7f35de46ee61175ac976699f/",
            "adbc_driver_snowflake-1.11.0-py3-none-manylinux1_x86_64.manylinux_2_28_x86_64.manylinux_2_5_x86_64.whl"
        ),
        sha256: "7c7d7ecef7246303648a08cee3d78c383133bea8f2e1568cf6e8750ab94b5a57",
        size: 27_574_063,
        path_in_archive: "adbc_driver_snowflake/libadbc_driver_snowflake.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "snowflake",
        library: "adbc_driver_snowflake",
        version: "1.11.0",
        os: "linux",
        arch: "aarch64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/5d/41/fe557f0d8414037dfe86fcba6338eb094a51da894050ec9a4c9f4504513d/",
            "adbc_driver_snowflake-1.11.0-py3-none-manylinux2014_aarch64.manylinux_2_17_aarch64.manylinux_2_28_aarch64.whl"
        ),
        sha256: "07b5edbeb1b85be0882d7095641df26a24b3b4c2ba4d8edda35e3bbe89e70a06",
        size: 24_767_291,
        path_in_archive: "adbc_driver_snowflake/libadbc_driver_snowflake.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "snowflake",
        library: "adbc_driver_snowflake",
        version: "1.11.0",
        os: "macos",
        arch: "aarch64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/d5/1d/88d564a8941607e6e555a3d3290ecb92852d4aa5a926a1cdbe86b17a8424/",
            "adbc_driver_snowflake-1.11.0-py3-none-macosx_11_0_arm64.whl"
        ),
        sha256: "be862f8d0b6a507dae5c0644e13be79290180532c097d97c5a95a294c0bb5302",
        size: 13_517_204,
        path_in_archive: "adbc_driver_snowflake/libadbc_driver_snowflake.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "snowflake",
        library: "adbc_driver_snowflake",
        version: "1.11.0",
        os: "macos",
        arch: "x86_64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/2a/f3/1125c7929faf750e7a5bdcec33ca22259ce040338dae12f75f3eb8080def/",
            "adbc_driver_snowflake-1.11.0-py3-none-macosx_10_15_x86_64.whl"
        ),
        sha256: "5ebddfa5fc0b5cd7c86b5cce17bb578889c8db5ff2b756903b64e684b56829e8",
        size: 15_094_744,
        path_in_archive: "adbc_driver_snowflake/libadbc_driver_snowflake.so",
        license: "Apache-2.0",
    },
    // ----------------------------------------------------------- BigQuery
    Artifact {
        driver: "bigquery",
        library: "adbc_driver_bigquery",
        version: "1.11.0",
        os: "windows",
        arch: "x86_64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/f2/41/595bdc39a6e8eea2be3ccd043a10e68243b8a682d45f52b5d982674269f3/",
            "adbc_driver_bigquery-1.11.0-py3-none-win_amd64.whl"
        ),
        sha256: "75ff98aa9071707419c300f9da74e19da375cd6524ecf09493c8131960e31c3c",
        size: 17_698_043,
        path_in_archive: "adbc_driver_bigquery/libadbc_driver_bigquery.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "bigquery",
        library: "adbc_driver_bigquery",
        version: "1.11.0",
        os: "linux",
        arch: "x86_64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/9e/0a/ce0b0772f479eb451993ed9a0d139fcdaddcab651d1ce09520003163aa54/",
            "adbc_driver_bigquery-1.11.0-py3-none-manylinux1_x86_64.manylinux_2_28_x86_64.manylinux_2_5_x86_64.whl"
        ),
        sha256: "3067b90ccbb6801936567bddcc56e79540efb7e09023f0300e9f6090cb7245e1",
        size: 18_043_100,
        path_in_archive: "adbc_driver_bigquery/libadbc_driver_bigquery.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "bigquery",
        library: "adbc_driver_bigquery",
        version: "1.11.0",
        os: "linux",
        arch: "aarch64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/74/2c/f71aaea78f4859935e334b1c5791e959cb24315201789bb9f7a8fd5a94c2/",
            "adbc_driver_bigquery-1.11.0-py3-none-manylinux2014_aarch64.manylinux_2_17_aarch64.manylinux_2_28_aarch64.whl"
        ),
        sha256: "dea28d663b622e1b4c803fd063b0b959dc867218461ecb6e34b557423e956527",
        size: 16_537_326,
        path_in_archive: "adbc_driver_bigquery/libadbc_driver_bigquery.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "bigquery",
        library: "adbc_driver_bigquery",
        version: "1.11.0",
        os: "macos",
        arch: "aarch64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/15/0b/b88fd632ea5b26ded6f11cbfc62d154249b54d4ae95ad8bdc2e681f3cbb0/",
            "adbc_driver_bigquery-1.11.0-py3-none-macosx_11_0_arm64.whl"
        ),
        sha256: "023744303c4900fc8e3d12cd044835d406ee8a84880458766aff0800601d4999",
        size: 9_049_987,
        path_in_archive: "adbc_driver_bigquery/libadbc_driver_bigquery.so",
        license: "Apache-2.0",
    },
    Artifact {
        driver: "bigquery",
        library: "adbc_driver_bigquery",
        version: "1.11.0",
        os: "macos",
        arch: "x86_64",
        url: concat!(
            "https://files.pythonhosted.org/packages",
            "/d6/0f/76baf47962bfa49608fcd5d38c3a46213155aeb1f58e5c8abf467ed7e4bc/",
            "adbc_driver_bigquery-1.11.0-py3-none-macosx_10_15_x86_64.whl"
        ),
        sha256: "4153bb759dcfa6c45420c4ee9a1505f3c5daa7685d10cf068a91b8876e183732",
        size: 9_736_982,
        path_in_archive: "adbc_driver_bigquery/libadbc_driver_bigquery.so",
        license: "Apache-2.0",
    },
];

/// The builds of `driver`, newest first.
pub fn artifacts(driver: &str) -> impl Iterator<Item = &'static Artifact> + '_ {
    ARTIFACTS.iter().filter(move |a| a.driver == driver)
}

/// The build of `driver` for this machine (`std::env::consts`).
pub fn for_current_platform(driver: &str) -> Option<&'static Artifact> {
    for_platform(driver, std::env::consts::OS, std::env::consts::ARCH)
}

pub fn for_platform(driver: &str, os: &str, arch: &str) -> Option<&'static Artifact> {
    artifacts(driver).find(|a| a.os == os && a.arch == arch)
}

/// The requirement shown in the driver descriptor (current platform; the
/// first build when the platform has none).
pub fn requirement(driver: &str) -> Option<Requirement> {
    let a = for_current_platform(driver).or_else(|| artifacts(driver).next())?;
    Some(Requirement::AdbcLibrary {
        name: a.library.to_owned(),
        version: a.version.to_owned(),
        sha256: a.sha256.to_owned(),
        license: a.license.to_owned(),
    })
}

/// File name the platform loader expects: `adbc_driver_x.dll`,
/// `libadbc_driver_x.so`, `libadbc_driver_x.dylib`.
pub fn library_file_name(library: &str, os: &str) -> String {
    match os {
        "windows" => format!("{library}.dll"),
        "macos" => format!("lib{library}.dylib"),
        _ => format!("lib{library}.so"),
    }
}

/// SHA-256 of a file, lower-case hex (verifying downloads).
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f =
        std::fs::File::open(path).map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Extracts `path_in_archive` from a zip archive (a wheel) into `dest_dir`
/// as `file_name`, through a temporary file renamed at the end (a partly
/// written library is never loaded). Returns the extracted path.
pub fn extract_library(
    archive: &Path,
    path_in_archive: &str,
    dest_dir: &Path,
    file_name: &str,
) -> Result<PathBuf> {
    let io = |e: &dyn std::fmt::Display| Error::Other(format!("{}: {e}", archive.display()));
    let file = std::fs::File::open(archive).map_err(|e| io(&e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| io(&e))?;
    let mut entry = zip
        .by_name(path_in_archive)
        .map_err(|e| io(&format!("{path_in_archive}: {e}")))?;
    std::fs::create_dir_all(dest_dir).map_err(|e| io(&e))?;
    let target = dest_dir.join(file_name);
    let tmp = dest_dir.join(format!("{file_name}.part"));
    {
        let mut out = std::fs::File::create(&tmp).map_err(|e| io(&e))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| io(&e))?;
        out.flush().map_err(|e| io(&e))?;
    }
    std::fs::rename(&tmp, &target).map_err(|e| io(&e))?;
    Ok(target)
}

/// Extracts the library of `artifact` for the current platform loader.
pub fn extract_artifact(artifact: &Artifact, archive: &Path, dest_dir: &Path) -> Result<PathBuf> {
    extract_library(
        archive,
        artifact.path_in_archive,
        dest_dir,
        &library_file_name(artifact.library, artifact.os),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_has_both_drivers() {
        for driver in ["snowflake", "bigquery"] {
            for (os, arch) in [
                ("windows", "x86_64"),
                ("linux", "x86_64"),
                ("linux", "aarch64"),
                ("macos", "aarch64"),
                ("macos", "x86_64"),
            ] {
                let a = for_platform(driver, os, arch).expect("build");
                assert!(a.url.starts_with(PYPI) && a.url.ends_with(".whl"));
                assert_eq!(a.sha256.len(), 64);
                assert!(a.path_in_archive.ends_with(".so"));
            }
            assert!(requirement(driver).is_some());
        }
        assert_eq!(
            library_file_name("adbc_driver_snowflake", "windows"),
            "adbc_driver_snowflake.dll"
        );
    }

    #[test]
    fn extracts_the_library_from_a_wheel() {
        // The SQLite driver wheels are in the repository root (phase-0 spike).
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let wheel = root.join("adbc_driver_sqlite-1.13.0-py3-none-win_amd64.whl");
        if !wheel.exists() {
            return;
        }
        assert_eq!(
            sha256_file(&wheel).unwrap(),
            "87396ae95c1d0a10d95ea644afb6ae9843a64148a09c3e56fa7870ee03944a22"
        );
        let dir = tempfile::tempdir().unwrap();
        let p = extract_library(
            &wheel,
            "adbc_driver_sqlite/libadbc_driver_sqlite.so",
            dir.path(),
            "adbc_driver_sqlite.dll",
        )
        .unwrap();
        let head = std::fs::read(&p).unwrap();
        assert_eq!(&head[..2], b"MZ");
        assert!(extract_library(&wheel, "missing.so", dir.path(), "x").is_err());
    }
}
