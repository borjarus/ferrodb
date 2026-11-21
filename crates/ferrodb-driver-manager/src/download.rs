//! Downloads (HTTPS through rustls + ring), SHA-256 verification and
//! archive extraction. Everything here is blocking: callers run it on a
//! blocking thread.

use std::{
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::ManagerError;

/// Progress callback: (bytes done, bytes total if known).
pub type Progress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .user_agent("FerroDb driver manager")
        .build()
        .into()
}

/// Lower-case hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = BufReader::new(File::open(path)?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// GETs a small JSON document (Adoptium API).
pub fn get_json(url: &str) -> Result<serde_json::Value, ManagerError> {
    let mut resp = agent()
        .get(url)
        .call()
        .map_err(|e| ManagerError::Network(format!("{url}: {e}")))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| ManagerError::Network(format!("{url}: {e}")))?;
    serde_json::from_str(&text).map_err(|e| ManagerError::Network(format!("{url}: {e}")))
}

/// Downloads `url` to `dest`, verifying `sha256` before the file appears
/// under its final name (fail closed: a mismatch leaves nothing behind).
pub fn download(
    url: &str,
    sha256: &str,
    dest: &Path,
    progress: Progress<'_>,
) -> Result<(), ManagerError> {
    if !url.starts_with("https://") {
        return Err(ManagerError::Untrusted(url.to_owned()));
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let part = dest.with_extension("part");
    let r = (|| {
        let mut resp = agent()
            .get(url)
            .call()
            .map_err(|e| ManagerError::Network(format!("{url}: {e}")))?;
        let total = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let mut body = resp.body_mut().with_config().limit(2 << 30).reader();
        let mut out = File::create(&part)?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        progress(0, total);
        loop {
            let n = body
                .read(&mut buf)
                .map_err(|e| ManagerError::Network(format!("{url}: {e}")))?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
        out.flush()?;
        drop(out);
        let got = hex(&h.finalize());
        if !got.eq_ignore_ascii_case(sha256) {
            return Err(ManagerError::Checksum {
                file: url.to_owned(),
                expected: sha256.to_owned(),
                actual: got,
            });
        }
        fs::rename(&part, dest)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = fs::remove_file(&part);
    }
    r
}

/// Copies a local file (offline installation), verifying its checksum.
pub fn install_file(src: &Path, sha256: Option<&str>, dest: &Path) -> Result<(), ManagerError> {
    if let Some(expected) = sha256 {
        let got = sha256_file(src)?;
        if !got.eq_ignore_ascii_case(expected) {
            return Err(ManagerError::Checksum {
                file: src.display().to_string(),
                expected: expected.to_owned(),
                actual: got,
            });
        }
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    if src != dest {
        fs::copy(src, dest)?;
    }
    Ok(())
}

fn is_tar_gz(name: &str) -> bool {
    name.ends_with(".tar.gz") || name.ends_with(".tgz")
}

/// Extracts `archive` (`.zip`, `.whl`, `.tar.gz`) into `dir`. With
/// `member`, only that file is kept and its path is returned; otherwise the
/// whole archive is unpacked and `dir` is returned.
pub fn extract(archive: &Path, dir: &Path, member: Option<&str>) -> Result<PathBuf, ManagerError> {
    fs::create_dir_all(dir)?;
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if is_tar_gz(&name) {
        let gz = flate2::read::GzDecoder::new(BufReader::new(File::open(archive)?));
        let mut tar = tar::Archive::new(gz);
        match member {
            None => {
                tar.unpack(dir)?;
                Ok(dir.to_owned())
            }
            Some(m) => {
                for entry in tar.entries()? {
                    let mut entry = entry?;
                    let path = entry.path()?.to_string_lossy().replace('\\', "/");
                    if path == m || path.ends_with(&format!("/{m}")) {
                        let out = dir.join(m.rsplit('/').next().unwrap_or(m));
                        entry.unpack(&out)?;
                        return Ok(out);
                    }
                }
                Err(ManagerError::Archive(format!("{m} not found in {name}")))
            }
        }
    } else {
        let mut zip = zip::ZipArchive::new(BufReader::new(File::open(archive)?))
            .map_err(|e| ManagerError::Archive(e.to_string()))?;
        match member {
            None => {
                zip.extract(dir)
                    .map_err(|e| ManagerError::Archive(e.to_string()))?;
                Ok(dir.to_owned())
            }
            Some(m) => {
                let mut f = zip
                    .by_name(m)
                    .map_err(|e| ManagerError::Archive(format!("{m}: {e}")))?;
                let out = dir.join(m.rsplit('/').next().unwrap_or(m));
                let mut w = File::create(&out)?;
                io::copy(&mut f, &mut w)?;
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_of_a_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.jar");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let dest = d.path().join("out/x.jar");
        assert!(matches!(
            install_file(&p, Some(&"0".repeat(64)), &dest),
            Err(ManagerError::Checksum { .. })
        ));
        assert!(!dest.exists(), "a mismatch writes nothing");
        install_file(
            &p,
            Some("BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"),
            &dest,
        )
        .unwrap();
        assert!(dest.exists());
    }

    #[test]
    fn plain_http_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let r = download(
            "http://example.invalid/x.jar",
            &"0".repeat(64),
            &d.path().join("x.jar"),
            &|_, _| {},
        );
        assert!(matches!(r, Err(ManagerError::Untrusted(_))));
    }

    #[test]
    fn zip_member_is_extracted() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.whl");
        {
            let mut w = zip::ZipWriter::new(File::create(&p).unwrap());
            w.start_file("pkg/lib.so", zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(b"MZ").unwrap();
            w.finish().unwrap();
        }
        let out = extract(&p, &d.path().join("x"), Some("pkg/lib.so")).unwrap();
        assert_eq!(fs::read(out).unwrap(), b"MZ");
    }
}
