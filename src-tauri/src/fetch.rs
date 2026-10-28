//! `kimiDesktop.fetchProviderModels`: the GET Kimi's settings page makes to
//! list a custom provider's models, run with `curl` on the host instead of in
//! the WebView. That gets around CORS (most model APIs do not allow browser
//! origins), and on an SSH host the request leaves from where Kimi Code will
//! actually call the provider, so endpoints only the host can reach (a vLLM on
//! its localhost, a cluster-internal name) work too.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::scripts;
use crate::transport::Transport;

/// Upper bound for the page's own timeout (it asks for 15s).
const MAX_TIMEOUT_MS: u64 = 60_000;

/// The request shape Kimi's page passes to the desktop bridge.
#[derive(Deserialize)]
pub struct Request {
    url: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(rename = "timeoutMs")]
    timeout_ms: Option<u64>,
}

/// Runs the request and answers in the bridge's shape:
/// `{ ok: true, status, body }`, or `{ ok: false, kind }` where `kind` is
/// `"timeout"` or anything else for a network failure.
pub async fn run(transport: &Transport, request: Request) -> Value {
    let config = match curl_config(&request) {
        Ok(config) => config,
        Err(detail) => return json!({ "ok": false, "kind": "network", "detail": detail }),
    };
    let lines = match transport.run_script(&scripts::fetch(&base64(config.as_bytes())), |_| {}).await {
        Ok(result) => result.lines,
        Err(detail) => return json!({ "ok": false, "kind": "network", "detail": detail }),
    };
    match parse(&lines) {
        Some(Ok((status, body))) => json!({ "ok": true, "status": status, "body": body }),
        Some(Err(28)) => json!({ "ok": false, "kind": "timeout" }),
        Some(Err(code)) => json!({ "ok": false, "kind": "network", "detail": format!("curl exited with {code}") }),
        None => json!({ "ok": false, "kind": "network", "detail": "no response from curl" }),
    }
}

/// A curl config (`curl -K`) for the request. Only http(s) GETs: the page
/// must not be able to read the host's files through `file://` and friends.
fn curl_config(request: &Request) -> Result<String, String> {
    let url = tauri::Url::parse(&request.url).map_err(|e| e.to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("unsupported URL scheme: {}", url.scheme()));
    }
    let timeout = request.timeout_ms.unwrap_or(15_000).clamp(1_000, MAX_TIMEOUT_MS);
    let mut config = format!(
        "silent\nshow-error\nlocation\nmax-filesize = 16777216\nmax-time = {:.1}\nurl = {}\n",
        timeout as f64 / 1000.0,
        quote(url.as_str())?
    );
    for (name, value) in &request.headers {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(format!("invalid header name: {name}"));
        }
        config.push_str(&format!("header = {}\n", quote(&format!("{name}: {value}"))?));
    }
    config.push_str(&format!("write-out = \"\\n{} %{{http_code}}\\n\"\n", scripts::FETCH_STATUS));
    Ok(config)
}

/// A double-quoted curl config string. Line breaks are refused rather than
/// escaped: they have no business in a URL or header.
fn quote(value: &str) -> Result<String, String> {
    if value.contains(['\r', '\n', '\0']) {
        return Err("line breaks are not allowed".into());
    }
    Ok(format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Picks the body and status out of the script's output (a login shell may
/// print its own lines before the body marker), or curl's exit code.
fn parse(lines: &[String]) -> Option<Result<(u16, String), i32>> {
    let start = lines.iter().position(|l| l == scripts::FETCH_BODY)? + 1;
    let exit: i32 = lines[start..].iter().rev().find_map(|l| l.strip_prefix(scripts::FETCH_EXIT))?.trim().parse().ok()?;
    if exit != 0 {
        return Some(Err(exit));
    }
    let end = start + lines[start..].iter().rposition(|l| l.starts_with(scripts::FETCH_STATUS))?;
    let status = lines[end].strip_prefix(scripts::FETCH_STATUS)?.trim().parse().ok()?;
    // The write-out starts with a newline of its own.
    let mut body = lines[start..end].join("\n");
    if lines[start..end].last().is_some_and(|l| l.is_empty()) {
        body.pop();
    }
    Some(Ok((status, body)))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str, headers: &[(&str, &str)]) -> Request {
        Request {
            url: url.into(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            timeout_ms: Some(15_000),
        }
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn builds_config() {
        let config = curl_config(&request("https://api.example.com/v1/models", &[("authorization", "Bearer a\"b\\c")])).unwrap();
        assert!(config.contains("url = \"https://api.example.com/v1/models\"\n"));
        assert!(config.contains("header = \"authorization: Bearer a\\\"b\\\\c\"\n"));
        assert!(config.contains("max-time = 15.0\n"));
    }

    #[test]
    fn refuses_unsafe_requests() {
        assert!(curl_config(&request("file:///etc/passwd", &[])).is_err());
        assert!(curl_config(&request("https://x/", &[("x-a", "b\r\nhost: evil")])).is_err());
        assert!(curl_config(&request("https://x/", &[("bad name", "v")])).is_err());
    }

    #[test]
    fn parses_output() {
        let lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let ok = lines(&["motd", scripts::FETCH_BODY, "{\"data\":[]}", "__KD_HTTP__ 200", "__KD_CURL__ 0"]);
        assert_eq!(parse(&ok), Some(Ok((200, "{\"data\":[]}".into()))));
        let trailing = lines(&[scripts::FETCH_BODY, "{}", "", "__KD_HTTP__ 401", "__KD_CURL__ 0"]);
        assert_eq!(parse(&trailing), Some(Ok((401, "{}".into()))));
        let timeout = lines(&[scripts::FETCH_BODY, "__KD_HTTP__ 000", "__KD_CURL__ 28"]);
        assert_eq!(parse(&timeout), Some(Err(28)));
        assert_eq!(parse(&lines(&["nothing"])), None);
    }
}
