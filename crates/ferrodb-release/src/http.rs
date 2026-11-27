//! Blocking HTTPS (ureq over rustls + ring), on blocking threads only.

use std::time::Duration;

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_body(Some(Duration::from_secs(300)))
        // No identifiers: the product name and version only.
        .user_agent(concat!("FerroDb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// `https://` always; plain `http://` only to loopback when allowed (tests).
pub(crate) fn check_url(url: &str, allow_loopback_http: bool) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|p| url.starts_with(p));
    if allow_loopback_http && loopback {
        Ok(())
    } else {
        Err(format!("refusing a non-HTTPS URL: {url}"))
    }
}

/// GETs a small document; `Ok(None)` on 404.
pub(crate) fn get_bytes(url: &str, limit: u64) -> Result<Option<Vec<u8>>, String> {
    match agent().get(url).call() {
        Ok(mut resp) => resp
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .map(Some)
            .map_err(|e| format!("{url}: {e}")),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(format!("{url}: {e}")),
    }
}
