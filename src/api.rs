//! Z-Library EAPI client (reverse-engineered protocol).
//!
//! Endpoints (all served by EAPI-capable mirrors such as z-lib.gd / 1lib.sk):
//!   GET  /eapi/info/ok                      -> {success:1}                    (health check)
//!   POST /eapi/user/login                   -> {success,user:{id,remix_userkey}}
//!   POST /eapi/book/search                  -> {success,books[],pagination}
//!   GET  /eapi/book/{id}/{hash}             -> {success,book{}}
//!   GET  /eapi/book/{id}/{hash}/file        -> {success,file:{downloadLink}}

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

/// Candidate EAPI mirrors. Probed concurrently at startup; first alive wins.
/// `.ws`/`.mx` are web-UI-only mirrors (no EAPI) but sessions are shared.
pub const DEFAULT_DOMAINS: &[&str] = &[
    "https://z-lib.gd",
    "https://1lib.sk",
    "https://z-library.sk",
    "https://z-lib.io",
    "https://z-lib.fm",
    "https://thai-books.sk",
    "https://spanish-books.sk",
    "https://frenchbooks.sk",
    "https://german-books.sk",
    "https://italian-books.sk",
    "https://portuguese-books.sk",
    "https://japanese-books.sk",
    "https://articles.sk",
];

pub const TOR_ONION: &str =
    "http://bookszlibb74ugqojhzhg2a63w5i2atv5bqarulgczawnbmsb6s6qead.onion";

#[derive(Clone, Debug)]
pub struct Session {
    pub domain: String,
    pub user_id: String,
    pub user_key: String,
    pub email: String,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Filters {
    pub exact: bool,
    pub year_from: String,
    pub year_to: String,
    pub language: String,
    pub extension: String,
    pub order: String,
}

#[derive(Clone, Debug, Default)]
pub struct Book {
    pub id: String,
    pub hash: String,
    pub title: String,
    pub author: String,
    pub year: String,
    pub publisher: String,
    pub language: String,
    pub extension: String,
    pub pages: String,
    pub size: String,
    pub description: String,
    pub cover: String,
    pub download_url: String,
}

#[derive(Clone, Debug, Default)]
pub struct SearchPage {
    pub books: Vec<Book>,
    pub page: u32,
    pub total_pages: u32,
}

// ---------- small JSON helpers (EAPI fields are loosely typed) ----------

fn vstr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

fn parse_book(item: &Value, domain: &str) -> Book {
    let id = vstr(get(item, "id"));
    let hash = vstr(get(item, "hash"));
    // The download link from details is /eapi/book/{id}/{hash}/file
    let download_url = format!("{}/eapi/book/{}/{}", domain, id, hash);
    let _ = download_url;
    Book {
        id,
        hash,
        title: vstr(get(item, "title")),
        author: vstr(get(item, "author")),
        year: vstr(get(item, "year")),
        publisher: vstr(get(item, "publisher")),
        language: vstr(get(item, "language")),
        extension: vstr(get(item, "extension")),
        pages: vstr(get(item, "pages")),
        size: vstr(get(item, "filesizeString")),
        description: vstr(get(item, "description")),
        cover: vstr(get(item, "cover")),
        download_url: String::new(),
    }
}

// ---------- HTTP plumbing ----------

pub fn build_client(proxy: &Option<String>, timeout: Option<Duration>) -> reqwest::blocking::Client {
    let mut b = reqwest::blocking::Client::builder()
        .user_agent("zlib-lite/0.1 (Z-Library desktop client)")
        .timeout(timeout.unwrap_or(Duration::from_secs(30)));
    if let Some(p) = proxy {
        if !p.is_empty() {
            b = match reqwest::Proxy::all(p) {
                Ok(px) => b.proxy(px),
                Err(_) => b,
            };
        }
    }
    b.build().unwrap_or_default()
}

fn auth_header(sess: &Session) -> String {
    format!("remix_userid={}; remix_userkey={}", sess.user_id, sess.user_key)
}

/// Probe one mirror: GET /eapi/info/ok must return {success:1}.
pub fn probe(domain: &str, proxy: &Option<String>) -> bool {
    let client = build_client(proxy, Some(Duration::from_secs(8)));
    let url = format!("{}/eapi/info/ok", normalize(domain));
    match client.get(&url).send() {
        Ok(resp) if resp.status().is_success() => match resp.json::<Value>() {
            Ok(j) => get(&j, "success").as_i64() == Some(1),
            _ => false,
        },
        _ => false,
    }
}

pub fn normalize(domain: &str) -> String {
    let d = domain.trim().trim_end_matches('/');
    if d.starts_with("http://") || d.starts_with("https://") {
        d.to_string()
    } else {
        format!("https://{}", d)
    }
}

/// Password login via EAPI. Returns the session on success.
pub fn login(
    domain: &str,
    proxy: &Option<String>,
    email: &str,
    password: &str,
) -> Result<Session, String> {
    let domain = normalize(domain);
    let client = build_client(proxy, None);
    let resp = client
        .post(format!("{}/eapi/user/login", domain))
        .header("desktop-app-version", "3.2.1")
        .header("desktop-app-os", std::env::consts::OS)
        .form(&[("email", email), ("password", password)])
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let status = resp.status();
    let json: Value = resp.json().map_err(|_| "invalid server response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        let err = vstr(get(&json, "error"));
        let msg = if err.is_empty() {
            format!("login rejected (HTTP {})", status.as_u16())
        } else {
            err
        };
        return Err(msg);
    }
    let user = get(&json, "user");
    let id = vstr(get(user, "id"));
    let mut key = vstr(get(user, "remix_userkey"));
    if key.is_empty() {
        key = vstr(get(user, "remix_user_key"));
    }
    if id.is_empty() || key.is_empty() {
        return Err("server did not return session credentials".into());
    }
    Ok(Session {
        domain,
        user_id: id,
        user_key: key,
        email: email.to_string(),
    })
}

/// Cookie-based login: accept pre-obtained remix_userid/remix_userkey
/// (e.g. exported from a working browser or app session at .ws).
pub fn login_with_cookies(
    domain: &str,
    proxy: &Option<String>,
    user_id: &str,
    user_key: &str,
) -> Result<Session, String> {
    let domain = normalize(domain);
    let sess = Session {
        domain,
        user_id: user_id.trim().to_string(),
        user_key: user_key.trim().to_string(),
        email: String::new(),
    };
    // Validate: user-info endpoint must accept the cookies.
    let client = build_client(proxy, None);
    let url = format!("{}/eapi/desktop-app/user-info/{}/{}", sess.domain, sess.user_id, sess.user_key);
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let json: Value = resp.json().map_err(|_| "invalid server response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        return Err("cookies rejected by server".into());
    }
    Ok(sess)
}

/// Create a new account. Triggers a confirmation code email.
pub fn register(
    domain: &str,
    proxy: &Option<String>,
    email: &str,
    password: &str,
    name: &str,
) -> Result<(), String> {
    let domain = normalize(domain);
    let client = build_client(proxy, None);
    let mut form: Vec<(&str, &str)> = vec![("email", email), ("password", password)];
    if !name.is_empty() {
        form.push(("name", name));
    }
    let resp = client
        .post(format!("{}/eapi/user/registration", domain))
        .header("desktop-app-version", "3.2.1")
        .header("desktop-app-os", std::env::consts::OS)
        .form(&form)
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let json: Value = resp.json().map_err(|_| "invalid server response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        return Err(err_or_default(&json, "registration rejected"));
    }
    Ok(())
}

/// Confirm the emailed code. On success returns a logged-in session.
pub fn confirm_code(
    domain: &str,
    proxy: &Option<String>,
    email: &str,
    password: &str,
    name: &str,
    code: &str,
) -> Result<Session, String> {
    let domain = normalize(domain);
    let client = build_client(proxy, None);
    let mut form: Vec<(&str, &str)> = vec![("email", email), ("password", password), ("code", code)];
    if !name.is_empty() {
        form.push(("name", name));
    }
    let resp = client
        .post(format!("{}/eapi/user/email/confirmation", domain))
        .header("desktop-app-version", "3.2.1")
        .header("desktop-app-os", std::env::consts::OS)
        .form(&form)
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let json: Value = resp.json().map_err(|_| "invalid server response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        return Err(err_or_default(&json, "confirmation rejected"));
    }
    let user = get(&json, "user");
    let id = vstr(get(user, "id"));
    let key = vstr(get(user, "remix_userkey"));
    if id.is_empty() || key.is_empty() {
        return Err("server did not return session credentials".into());
    }
    Ok(Session {
        domain,
        user_id: id,
        user_key: key,
        email: email.to_string(),
    })
}

/// Resend the confirmation code email.
pub fn resend_code(
    domain: &str,
    proxy: &Option<String>,
    email: &str,
    password: &str,
    name: &str,
) -> Result<(), String> {
    let domain = normalize(domain);
    let client = build_client(proxy, None);
    let mut form: Vec<(&str, &str)> = vec![("email", email), ("password", password)];
    if !name.is_empty() {
        form.push(("name", name));
    }
    let resp = client
        .post(format!("{}/eapi/user/email/confirmation/resend", domain))
        .header("desktop-app-version", "3.2.1")
        .header("desktop-app-os", std::env::consts::OS)
        .form(&form)
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let json: Value = resp.json().map_err(|_| "invalid server response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        return Err(err_or_default(&json, "resend rejected"));
    }
    Ok(())
}

fn err_or_default(json: &Value, default: &str) -> String {
    let e = vstr(get(json, "error"));
    if e.is_empty() {
        default.to_string()
    } else {
        e
    }
}

/// Search books. POST form: message, page, limit, e, yearFrom, yearTo,
/// languages[], extensions[], order.
pub fn search(
    sess: &Session,
    proxy: &Option<String>,
    query: &str,
    page: u32,
    filters: &Filters,
) -> Result<SearchPage, String> {
    let client = build_client(proxy, None);
    let mut form: Vec<(&str, String)> = vec![
        ("message", query.to_string()),
        ("page", page.to_string()),
        ("limit", "50".to_string()),
    ];
    if filters.exact {
        form.push(("e", "1".into()));
    }
    let yf = filters.year_from.trim();
    let yt = filters.year_to.trim();
    if !yf.is_empty() {
        form.push(("yearFrom", yf.to_string()));
    }
    if !yt.is_empty() {
        form.push(("yearTo", yt.to_string()));
    }
    let lang = filters.language.trim().to_lowercase();
    if !lang.is_empty() {
        form.push(("languages[]", lang));
    }
    let ext = filters.extension.trim().to_lowercase();
    if !ext.is_empty() && ext != "any" {
        form.push(("extensions[]", ext));
    }
    let order = filters.order.trim();
    if !order.is_empty() && order != "bestmatch" {
        form.push(("order", order.to_string()));
    }

    let resp = client
        .post(format!("{}/eapi/book/search", sess.domain))
        .header("Cookie", auth_header(sess))
        .form(&form)
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let status = resp.status();
    let json: Value = resp.json().map_err(|_| "invalid server response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        let err = vstr(get(&json, "error"));
        return Err(if err.is_empty() {
            format!("search rejected (HTTP {})", status.as_u16())
        } else {
            err
        });
    }
    let mut books = Vec::new();
    if let Some(arr) = get(&json, "books").as_array() {
        for item in arr {
            books.push(parse_book(item, &sess.domain));
        }
    }
    let pag = get(&json, "pagination");
    let page = get(pag, "current").as_u64().unwrap_or(page as u64) as u32;
    let total_pages = get(pag, "total_pages").as_u64().unwrap_or(1) as u32;
    Ok(SearchPage { books, page, total_pages: total_pages.max(1) })
}

/// Book details via /eapi/book/{id}/{hash}.
pub fn book_details(sess: &Session, proxy: &Option<String>, id: &str, hash: &str) -> Result<Book, String> {
    let client = build_client(proxy, None);
    let url = format!("{}/eapi/book/{}/{}", sess.domain, id, hash);
    let resp = client
        .get(&url)
        .header("Cookie", auth_header(sess))
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let json: Value = resp.json().map_err(|_| "invalid server response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        return Err(vstr(get(&json, "error")).is_empty()
            .then(|| "book request rejected".to_string())
            .unwrap_or_else(|| vstr(get(&json, "error"))));
    }
    let mut book = parse_book(get(&json, "book"), &sess.domain);
    book.id = if book.id.is_empty() { id.to_string() } else { book.id };
    book.hash = if book.hash.is_empty() { hash.to_string() } else { book.hash };
    Ok(book)
}

/// Resolve /eapi/book/{id}/{hash}/file to a CDN download link.
fn resolve_file_link(sess: &Session, client: &reqwest::blocking::Client, book: &Book) -> Result<String, String> {
    let url = format!("{}/eapi/book/{}/file", sess.domain, book.id);
    // Wait: the file path is /eapi/book/{id}/{hash}/file
    let url = format!("{}/eapi/book/{}/{}/file", sess.domain, book.id, book.hash);
    let resp = client
        .get(&url)
        .header("Cookie", auth_header(sess))
        .send()
        .map_err(|e| format!("network error: {e}"))?;
    let json: Value = resp.json().map_err(|_| "invalid file response".to_string())?;
    if get(&json, "success").as_i64() != Some(1) {
        return Err(vstr(get(&json, "error")).is_empty()
            .then(|| "file request rejected".to_string())
            .unwrap_or_else(|| vstr(get(&json, "error"))));
    }
    let file = get(&json, "file");
    for candidate in [
        vstr(get(file, "downloadLink")),
        vstr(get(&json, "downloadLink")),
        vstr(get(&json, "url")),
        vstr(get(&json, "link")),
    ] {
        if !candidate.is_empty() {
            return Ok(if candidate.starts_with("http") {
                candidate
            } else {
                format!("{}{}", sess.domain, candidate)
            });
        }
    }
    Err("server returned no download link".into())
}

fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    s = s.trim().trim_matches('.').to_string();
    if s.is_empty() {
        s = "book".into();
    }
    // cap length
    if s.len() > 180 {
        s.truncate(180);
    }
    s
}

fn filename_from_response(resp: &reqwest::blocking::Response, fallback: &str) -> String {
    let cd = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    for pattern in ["filename*=UTF-8''", "filename="] {
        if let Some(pos) = cd.find(pattern) {
            let raw = &cd[pos + pattern.len()..];
            let raw = raw.trim().trim_matches('"');
            let raw = raw.split(';').next().unwrap_or(raw);
            let decoded = percent_decode(raw);
            if !decoded.is_empty() {
                return sanitize_filename(&decoded);
            }
        }
    }
    sanitize_filename(fallback)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 <= bytes.len() - 1 + 1 {
            if i + 2 < bytes.len() || i + 2 == bytes.len() - 0 {
                // enough data
            }
            if i + 2 <= bytes.len() - 1 {
                let hex = &s[i + 1..i + 3];
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Download a book into dest_dir. Streams to a temp file and renames on
/// completion. `progress` is called with (bytes_written, total_bytes_option).
pub fn download_book(
    sess: &Session,
    proxy: &Option<String>,
    book: &Book,
    dest_dir: &std::path::Path,
    mut progress: impl FnMut(u64, Option<u64>) + Send,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("cannot create download dir: {e}"))?;
    let client = build_client(proxy, None);

    let link = resolve_file_link(sess, &client, book)?;

    // Send session cookies only when the link stays on the account's domain.
    let same_host = host_of(&link) == host_of(&sess.domain);
    let mut req = client.get(&link);
    if same_host {
        req = req.header("Cookie", auth_header(sess));
    }
    let resp = req.send().map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status().as_u16()));
    }

    let total = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let fallback_name = format!(
        "{}.{}",
        book.title,
        if book.extension.is_empty() { "bin".into() } else { book.extension.clone() }
    );
    let filename = filename_from_response(&resp, &fallback_name);
    let dest_path = dest_dir.join(filename);
    let tmp_path = dest_dir.join(format!(".zlib-lite-{}.part", std::process::id()));

    let mut file = std::fs::File::create(&tmp_path).map_err(|e| format!("cannot write file: {e}"))?;
    let mut reader = resp;
    let mut buf = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download error: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("write error: {e}"))?;
        written += n as u64;
        progress(written, total);
    }
    drop(file);
    std::fs::rename(&tmp_path, &dest_path).map_err(|e| format!("cannot finish file: {e}"))?;
    Ok(dest_path)
}

fn host_of(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split('/').next().unwrap_or("").to_lowercase()
}

/// Parse a Netscape cookie file, extracting remix_userid / remix_userkey.
pub fn parse_netscape_cookies(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() >= 7 && parts.last().is_some() {
            let name = parts[parts.len() - 2].to_string();
            let value = parts[parts.len() - 1].to_string();
            if name == "remix_userid" || name == "remix_userkey" {
                map.insert(name, value);
            }
        }
    }
    map
}
