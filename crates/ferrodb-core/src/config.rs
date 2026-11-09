//! `data-sources.toml` in the configuration directory (plan, section 5.10).
//! Holds no secrets: those are referenced as `secret://<id>`.

use std::path::Path;

use ferrodb_api::{DataSourceConfig, Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "data_source")]
    sources: Vec<DataSourceConfig>,
}

pub fn load(path: &Path) -> Result<Vec<DataSourceConfig>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Other(format!("{}: {e}", path.display()))),
    };
    let file: File =
        toml::from_str(&text).map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    Ok(file.sources)
}

/// Writes atomically (temporary file + rename).
pub fn save(path: &Path, sources: &[DataSourceConfig]) -> Result<()> {
    let io = |e: std::io::Error| Error::Other(format!("{}: {e}", path.display()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let text = toml::to_string_pretty(&File {
        sources: sources.to_vec(),
    })
    .map_err(|e| Error::Other(e.to_string()))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(
        &tmp,
        format!("# FerroDb data sources. Secrets live in the system keyring.\n{text}"),
    )
    .map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("data-sources.toml");
        assert!(load(&path).unwrap().is_empty());
        let ds = DataSourceConfig {
            id: "pg-1".into(),
            name: "Local \"pg\"".into(),
            driver: "postgresql".into(),
            folder: vec!["work".into()],
            environment: ferrodb_api::Environment::Production,
            read_only: true,
            tx_mode: ferrodb_api::TxMode::Manual,
            properties: vec![("host".into(), "localhost".into())],
            secret_ref: Some("secret://pg-1".into()),
        };
        save(&path, std::slice::from_ref(&ds)).unwrap();
        assert_eq!(load(&path).unwrap(), vec![ds]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("secret://pg-1"));
    }
}
