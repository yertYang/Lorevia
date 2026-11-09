//! Public web snapshots. Every redirect is resolved, checked and pinned before
//! connecting; imported HTML is converted to inert text and never executed.
use crate::{Error, Result};
use futures_util::StreamExt;
use reqwest::{Url, header};
use scraper::{Html, Selector, node::Node};
use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};
const LIMIT: usize = 2 * 1024 * 1024;
static FETCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
#[derive(Clone)]
pub struct Snapshot {
    pub requested_url: String,
    pub url: String,
    pub title: String,
    pub content: String,
    pub original: Vec<u8>,
    pub mime: String,
}
fn invalid() -> Error {
    Error::invalid("Use a public HTTP or HTTPS page without credentials, on a standard web port.")
}
fn v4_public(ip: Ipv4Addr) -> bool {
    let n = u32::from(ip);
    let blocked = [
        (0x00000000, 8),
        (0x0a000000, 8),
        (0x64400000, 10),
        (0x7f000000, 8),
        (0xa9fe0000, 16),
        (0xac100000, 12),
        (0xc0000000, 24),
        (0xc0000200, 24),
        (0xc0586300, 24),
        (0xc0a80000, 16),
        (0xc6120000, 15),
        (0xc6336400, 24),
        (0xcb007100, 24),
        (0xe0000000, 3),
    ];
    !blocked
        .iter()
        .any(|(base, bits)| n >> (32 - bits) == base >> (32 - bits))
}
pub(crate) fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_public(ip),
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return v4_public(ip);
            }
            let n = u128::from(ip);
            n >> 125 == 1
                && n >> 96 != 0x20010db8
                && n >> 112 != 0x2002
                && n >> 105 != 0x20010000000000000000000000000000u128 >> 105
        }
    }
}
fn url(input: &str) -> Result<Url> {
    if input.len() > 4096 || input.chars().any(char::is_control) {
        return Err(invalid());
    }
    let mut u = Url::parse(input.trim()).map_err(|_| invalid())?;
    if !["http", "https"].contains(&u.scheme())
        || !u.username().is_empty()
        || u.password().is_some()
        || ![Some(80), Some(443)].contains(&u.port_or_known_default())
    {
        return Err(invalid());
    }
    let host = u.host_str().ok_or_else(invalid)?.trim_matches(['[', ']']);
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.parse::<IpAddr>().is_ok_and(|ip| !public(ip))
    {
        return Err(invalid());
    }
    u.set_fragment(None);
    Ok(u)
}
pub fn canonical_url(input: &str) -> Result<String> {
    Ok(url(input)?.to_string())
}
fn extract(bytes: &[u8], mime: &str, address: &str) -> Result<(String, String)> {
    let charset = mime.split(';').find_map(|p| {
        p.trim()
            .split_once('=')
            .filter(|(key, _)| key.eq_ignore_ascii_case("charset"))
            .map(|(_, value)| value.trim_matches(['\'', '"']))
    });
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(2048)]).to_ascii_lowercase();
    let meta = head
        .find("charset")
        .and_then(|i| head[i + 7..].trim_start().strip_prefix('='))
        .map(|s| {
            s.trim_start()
                .trim_start_matches(['\'', '"'])
                .split(['\'', '"', '>', ';', ' ', '\t', '\n', '\r'])
                .next()
                .unwrap_or("")
        });
    let label = charset.or(meta);
    let encoding = match label {
        Some(label) => encoding_rs::Encoding::for_label(label.as_bytes())
            .ok_or_else(|| Error::invalid("This page uses an unsupported text encoding."))?,
        None => encoding_rs::UTF_8,
    };
    let (html, _, errors) = encoding.decode(bytes);
    if errors || html.trim().is_empty() || html.len() > LIMIT || html.contains('\0') {
        return Err(Error::invalid(
            "The page text could not be decoded or exceeds the reading limit. Save it as UTF-8 and import the file.",
        ));
    }
    if mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("text/plain")
    {
        return Ok((address.into(), html.into_owned()));
    }
    let document = Html::parse_document(&html);
    let selector = |s| Selector::parse(s).expect("constant HTML selector");
    let title = document
        .select(&selector("title"))
        .next()
        .map(|n| {
            n.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| address.into());
    let root = document
        .select(&selector("article"))
        .next()
        .or_else(|| document.select(&selector("main")).next())
        .or_else(|| document.select(&selector("body")).next())
        .ok_or_else(|| Error::invalid("This page has no readable content."))?;
    let mut lines = Vec::new();
    let mut pending = String::new();
    for node in root.descendants() {
        let skipped=node.ancestors().any(|a|matches!(a.value(),Node::Element(e) if ["script","style","noscript","template","nav","header","footer","form","button","iframe","svg","canvas"].contains(&e.name())));
        if skipped {
            continue;
        }
        match node.value() {
            Node::Element(e)
                if [
                    "p",
                    "div",
                    "section",
                    "article",
                    "main",
                    "h1",
                    "h2",
                    "h3",
                    "h4",
                    "li",
                    "br",
                    "tr",
                    "pre",
                    "blockquote",
                ]
                .contains(&e.name()) =>
            {
                if !pending.trim().is_empty() {
                    lines.push(pending.split_whitespace().collect::<Vec<_>>().join(" "));
                    pending.clear();
                }
            }
            Node::Text(t) => pending.push_str(t),
            _ => {}
        }
    }
    if !pending.trim().is_empty() {
        lines.push(pending.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    let content = lines.join("\n");
    if content.trim().is_empty() || content.len() > LIMIT {
        return Err(Error::invalid(
            "This page has no readable text or exceeds 2 MiB. Import a saved document instead.",
        ));
    }
    let mut title = title;
    while title.len() > 1000 {
        title.pop();
    }
    Ok((title, content))
}
async fn fetch_inner(input: &str) -> Result<Snapshot> {
    let requested = url(input)?;
    let mut current = requested.clone();
    for redirects in 0..=5 {
        let host = current
            .host_str()
            .ok_or_else(invalid)?
            .trim_matches(['[', ']']);
        let port = current.port_or_known_default().ok_or_else(invalid)?;
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| Error::provider("Unable to resolve this web page."))?
            .collect::<Vec<_>>();
        if addresses.is_empty() || addresses.len() > 32 || addresses.iter().any(|a| !public(a.ip()))
        {
            return Err(invalid());
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve_to_addrs(host, &addresses)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| Error::provider("Unable to initialize the web reader."))?;
        let response=client.get(current.clone()).header(header::USER_AGENT,"LoreviaReading/1.0").header(header::ACCEPT,"text/html,application/xhtml+xml,text/plain").send().await.map_err(|_|Error::provider("Unable to fetch this web page. It may require a browser login; import a saved document instead."))?;
        if response.status().is_redirection() {
            if redirects == 5 {
                return Err(Error::invalid("This page redirects too many times."));
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|s| s.to_str().ok())
                .ok_or_else(|| Error::invalid("The page returned an invalid redirect."))?;
            current = url(current.join(location).map_err(|_| invalid())?.as_str())?;
            continue;
        }
        if !response.status().is_success() {
            return Err(Error::provider(format!(
                "The web page returned HTTP {}. Import a saved document if access requires a browser.",
                response.status().as_u16()
            )));
        }
        let mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if !["text/html", "application/xhtml+xml", "text/plain"].contains(
            &mime
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str(),
        ) {
            return Err(Error::invalid(
                "This URL is not an HTML or text page. Download and import supported documents instead.",
            ));
        }
        if response.content_length().is_some_and(|n| n > LIMIT as u64) {
            return Err(Error::invalid("Web page exceeds 2 MiB."));
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| Error::provider("The web page download was interrupted."))?;
            if bytes.len() + chunk.len() > LIMIT {
                return Err(Error::invalid("Web page exceeds 2 MiB."));
            }
            bytes.extend_from_slice(&chunk);
        }
        let (title, content) = extract(&bytes, &mime, current.as_str())?;
        return Ok(Snapshot {
            requested_url: requested.to_string(),
            url: current.to_string(),
            title,
            content,
            original: bytes,
            mime,
        });
    }
    Err(Error::invalid("This page redirects too many times."))
}
pub async fn fetch(input: &str) -> Result<Snapshot> {
    let _permit = FETCHES
        .try_acquire()
        .map_err(|_| Error::conflict("Two web imports are running. Retry when they finish."))?;
    tokio::time::timeout(Duration::from_secs(30), fetch_inner(input))
        .await
        .map_err(|_| Error::provider("The web page took too long to load."))?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_nonpublic_addresses_credentials_schemes_and_ports() {
        for s in [
            "file:///etc/passwd",
            "http://localhost",
            "http://foo.local",
            "http://127.1/",
            "http://2130706433/",
            "http://0x7f000001/",
            "http://10.0.0.1/",
            "http://169.254.169.254/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "https://name:secret@example.com/",
            "http://example.com:8001/",
            "http://[64:ff9b::7f00:1]/",
        ] {
            assert!(url(s).is_err(), "{s}");
        }
        for s in [
            "https://example.com/page#part",
            "http://8.8.8.8/",
            "https://[2606:4700:4700::1111]/",
        ] {
            assert!(url(s).is_ok(), "{s}");
        }
    }
    #[test]
    fn removes_active_and_navigation_content_without_executing_it() {
        let(title,text)=extract(b"<html><title>Reading &amp; notes</title><body><nav>Ignore</nav><article><h1>Topic</h1><p>Energy <b>is</b> conserved.</p><script>fetch('/private')</script><form>Secrets</form><p>Second &lt;line&gt;.</p></article></body></html>","text/html; charset=utf-8","https://example.com/").unwrap();
        assert_eq!(title, "Reading & notes");
        assert_eq!(text, "Topic\nEnergy is conserved.\nSecond <line>.");
    }
    #[test]
    fn decodes_supported_charsets_and_rejects_empty_or_corrupt_text() {
        let (bytes, _, _) = encoding_rs::GBK.encode("<html><body><p>能量守恒</p></body></html>");
        assert_eq!(
            extract(&bytes, "text/html; charset=gbk", "https://example.com")
                .unwrap()
                .1,
            "能量守恒"
        );
        assert!(extract(b"<script>hi</script>", "text/html", "https://example.com").is_err());
        assert!(
            extract(
                &[0xff, 0xfe],
                "text/plain; charset=utf-8",
                "https://example.com"
            )
            .is_err()
        );
    }
}
