//! A minimal HTTPS client for the few cloud authentication calls drivers
//! make before connecting: Redshift `GetClusterCredentials`, Microsoft
//! Entra ID token requests. One request per connection (`Connection:
//! close`), rustls with the platform verifier; no proxies, no redirects.
//! A full HTTP stack (and the AWS / Azure SDKs with it) would add a large
//! dependency tree, and the SDKs default to `aws-lc-rs` (cmake / NASM on
//! Windows).

use std::{sync::Arc, time::Duration};

use ferrodb_api::{Error, Result};
use ferrodb_net::TlsMode;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
};

/// A response: status code, headers (lowercase names) and body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// `https://host[:port]/path?query` → (host, port, path with query).
pub(crate) fn split_url(url: &str) -> Result<(String, u16, String)> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::Connection(format!("not an https URL: {url}")))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse()
                .map_err(|_| Error::Connection(format!("bad port in {url}")))?,
        ),
        None => (authority, 443),
    };
    Ok((host.to_owned(), port, path.to_owned()))
}

/// POSTs `body` to `url` with `headers` (`Host`, `Content-Length` and
/// `Connection` are added).
pub async fn post(
    url: &str,
    headers: &[(&str, String)],
    body: &str,
    timeout: Duration,
) -> Result<Response> {
    tokio::time::timeout(timeout, post_inner(url, headers, body))
        .await
        .map_err(|_| Error::Connection(format!("{url}: timed out")))?
}

async fn post_inner(url: &str, headers: &[(&str, String)], body: &str) -> Result<Response> {
    let (host, port, path) = split_url(url)?;
    let fail = |e: &dyn std::fmt::Display| Error::Connection(format!("{host}: {e}"));
    let config = ferrodb_net::client_config(TlsMode::VerifyFull, None)
        .map_err(|e| fail(&e))?
        .ok_or_else(|| fail(&"TLS unavailable"))?;
    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| fail(&e))?;
    let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| fail(&e))?;
    let connector = tokio_rustls::TlsConnector::from(Arc::<rustls::ClientConfig>::clone(&config));
    let mut tls = connector.connect(name, tcp).await.map_err(|e| fail(&e))?;
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    tls.write_all(req.as_bytes()).await.map_err(|e| fail(&e))?;
    tls.flush().await.map_err(|e| fail(&e))?;
    let mut raw = Vec::new();
    match tls.read_to_end(&mut raw).await {
        Ok(_) => {}
        // Servers often close without a TLS close_notify.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !raw.is_empty() => {}
        Err(e) => return Err(fail(&e)),
    }
    parse_response(&raw).ok_or_else(|| fail(&"malformed HTTP response"))
}

/// Parses an HTTP/1.1 response read to the end of the connection.
pub(crate) fn parse_response(raw: &[u8]) -> Option<Response> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let mut body = &raw[split + 4..];
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let mut decoded = Vec::new();
    if chunked {
        loop {
            let eol = body.windows(2).position(|w| w == b"\r\n")?;
            let size_text = std::str::from_utf8(&body[..eol]).ok()?;
            let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
            body = &body[eol + 2..];
            if size == 0 {
                break;
            }
            decoded.extend_from_slice(body.get(..size)?);
            body = body.get(size + 2..).unwrap_or_default();
        }
    } else {
        decoded.extend_from_slice(body);
    }
    Some(Response {
        status,
        headers,
        body: String::from_utf8_lossy(&decoded).into_owned(),
    })
}

/// `application/x-www-form-urlencoded` / RFC 3986 unreserved encoding
/// (also what AWS Signature V4 expects).
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `k1=v1&k2=v2`, encoded.
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Text of the first `<tag>…</tag>` element of an XML document, with the
/// five predefined entities decoded (AWS Query API responses).
pub fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(
        xml[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_and_encoding() {
        let r = parse_response(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!((r.status, r.body.as_str()), (200, "hello world"));
        let r = parse_response(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\n\r\nno").unwrap();
        assert!(!r.ok());
        assert_eq!(r.body, "no");
        assert_eq!(url_encode("a b/c=d~"), "a%20b%2Fc%3Dd~");
        assert_eq!(form(&[("Action", "X"), ("Db", "a&b")]), "Action=X&Db=a%26b");
        assert_eq!(
            split_url("https://login.example.com/t/oauth2?x=1").unwrap(),
            ("login.example.com".into(), 443, "/t/oauth2?x=1".into())
        );
        assert_eq!(
            xml_text("<a><DbPassword>p&amp;w</DbPassword></a>", "DbPassword").as_deref(),
            Some("p&w")
        );
    }
}
