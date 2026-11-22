//! `ai.toml` in the configuration directory and the API keys in the secret
//! store (`secret://ai-<provider>`, key `api_key`). Everything here may block
//! (file system, keyring): callers run it with `spawn_blocking`.

use std::path::Path;

use ferrodb_api::{AiProvider, AiSettings};
use ferrodb_secrets::SecretStore;

use super::AiError;

pub(crate) const FILE: &str = "ai.toml";
const KEY: &str = "api_key";

fn slug(provider: AiProvider) -> &'static str {
    match provider {
        AiProvider::Anthropic => "anthropic",
        AiProvider::OpenAiCompatible => "openai-compatible",
        AiProvider::Ollama => "ollama",
    }
}

/// `secret://ai-<provider>`.
pub(crate) fn key_reference(provider: AiProvider) -> String {
    ferrodb_secrets::reference(&format!("ai-{}", slug(provider)))
}

/// The stored settings; defaults when the file is missing or unreadable.
pub(crate) fn load(dir: Option<&Path>) -> AiSettings {
    let Some(dir) = dir else {
        return AiSettings::default();
    };
    let path = dir.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{}: {e}; using default AI settings", path.display());
            AiSettings::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AiSettings::default(),
        Err(e) => {
            tracing::warn!("{}: {e}; using default AI settings", path.display());
            AiSettings::default()
        }
    }
}

/// Writes `ai.toml` (through a temporary file, so a crash never leaves half
/// a file). `has_key` is never written.
pub(crate) fn save(dir: &Path, settings: &AiSettings) -> Result<(), AiError> {
    let text = toml::to_string_pretty(settings).map_err(|e| AiError(e.to_string()))?;
    let fail = |e: std::io::Error| AiError(format!("{}: {e}", dir.join(FILE).display()));
    std::fs::create_dir_all(dir).map_err(fail)?;
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, dir.join(FILE)).map_err(fail)
}

/// The API key of `provider`, `None` when none is stored.
pub(crate) fn read_key(
    store: &dyn SecretStore,
    provider: AiProvider,
) -> Result<Option<String>, AiError> {
    let secrets = store
        .get(&key_reference(provider))
        .map_err(|e| AiError(format!("Cannot read the API key: {e}")))?;
    Ok(secrets.get(KEY).filter(|k| !k.is_empty()).cloned())
}

/// Stores the API key of `provider`; an empty key removes it.
pub(crate) fn write_key(
    store: &dyn SecretStore,
    provider: AiProvider,
    key: &str,
) -> Result<(), AiError> {
    let reference = key_reference(provider);
    let fail = |e: ferrodb_secrets::SecretError| AiError(format!("Cannot store the API key: {e}"));
    let mut secrets = store.get(&reference).map_err(fail)?;
    if key.is_empty() {
        secrets.remove(KEY);
    } else {
        secrets.insert(KEY.to_owned(), key.to_owned());
    }
    store.set(&reference, &secrets).map_err(fail)
}
