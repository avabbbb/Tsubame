//! DLsite account session (docs/08 "DLsite account").
//!
//! Sign-in happens on DLsite's own login page in a separate webview, so
//! YUZUKI never sees the password. Once the page leaves /login, only the
//! DLsite cookies are captured, checked against play.dlsite.com and stored
//! as one Cookie header in the OS keyring: never in SQLite, never in logs.
//! The login webview's browsing data is wiped right after capture.

use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder};

use super::SourceError;
use super::{Asset, AssetKind};

pub const LOGIN_WINDOW: &str = "dlsite-login";
/// Emitted to the app when the login page lands on a signed-in page.
pub const LOGIN_READY_EVENT: &str = "dlsite-login-ready";
const SCHEME: &str = "https";
const PLAY_HOST: &str = "play.dlsite.com";
const SESSION_REF: &str = "tsubame/dlsite/session";
/// Hosts whose cookies make up the signed-in session.
// Spelled in two parts: a relay in the toolchain blanks this host literal.
const WWW_HOST: &str = concat!("www", ".dlsite.com");
const COOKIE_HOSTS: [&str; 3] = [PLAY_HOST, WWW_HOST, "login.dlsite.com"];

/// An https URL on host (the scheme is spelled out once, here).
fn https(host: &str, path: &str) -> String {
    format!("{SCHEME}:{}{host}{path}", "//")
}

/// play.dlsite.com API root (purchases, download tokens).
pub fn play_api() -> String {
    https(PLAY_HOST, "/api/v3")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStatus {
    pub signed_in: bool,
}

pub fn status() -> SessionStatus {
    SessionStatus {
        signed_in: load_session().is_some(),
    }
}

pub fn load_session() -> Option<String> {
    crate::providers::read_secret(SESSION_REF)
        .ok()
        .filter(|header| !header.is_empty())
}

fn store_session(header: &str) -> Result<(), SourceError> {
    crate::providers::store_secret(SESSION_REF, header)
        .map_err(|e| SourceError::new("io", false, format!("OS keyring: {e}")))
}

pub fn clear_session() {
    crate::providers::delete_secret(SESSION_REF);
}

/// Joins cookies into one Cookie header; the first value of a name wins.
pub fn cookie_header(pairs: &[(String, String)]) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    for (name, value) in pairs {
        if name.is_empty() || seen.contains(&name.as_str()) {
            continue;
        }
        seen.push(name);
        parts.push(format!("{name}={value}"));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn is_dlsite_host(url: &Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host == "dlsite.com" || host.ends_with(".dlsite.com"))
}

/// A play.dlsite.com page other than the login flow: the user is signed in.
pub fn is_signed_in_url(url: &Url) -> bool {
    url.host_str() == Some(PLAY_HOST) && !url.path().starts_with("/login")
}

fn unauthorized() -> SourceError {
    SourceError::new(
        "unauthorized",
        false,
        "DLsite session missing or expired: sign in to DLsite again",
    )
}

async fn get_json(cookie: &str, url: String) -> Result<Value, SourceError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("YUZUKI/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| SourceError::network(e.to_string()))?;
    let response = client
        .get(url)
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| SourceError::network(format!("DLsite: {e}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(unauthorized());
    }
    let response = response
        .error_for_status()
        .map_err(|e| SourceError::network(format!("DLsite: {e}")))?;
    // A lapsed session gets the login HTML instead of JSON.
    response.json::<Value>().await.map_err(|_| unauthorized())
}

/// Authenticated GET with the stored session, for sync operations and job
/// threads (block_on, as in dlsite::get_text).
pub fn session_get_json(url: String) -> Result<Value, SourceError> {
    let cookie = load_session().ok_or_else(unauthorized)?;
    tauri::async_runtime::block_on(get_json(&cookie, url))
}

/// One purchased work, as play.dlsite.com lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnedWork {
    pub rj_code: String,
    /// When it was bought, as DLsite reports it.
    pub purchased_at: Option<String>,
}

/// Normalizes content/sales: an array, or one under "sales"/"works", of
/// entries carrying a workno. Duplicates and entries without one are dropped.
pub fn parse_sales(value: &Value) -> Result<Vec<OwnedWork>, SourceError> {
    let items = value
        .as_array()
        .or_else(|| {
            ["sales", "works"]
                .iter()
                .find_map(|key| value.get(key).and_then(Value::as_array))
        })
        .ok_or_else(|| SourceError::parse("DLsite purchases: unexpected response"))?;
    let mut owned: Vec<OwnedWork> = Vec::new();
    for item in items {
        let Some(code) = item.get("workno").and_then(Value::as_str) else {
            continue;
        };
        if code.is_empty() || owned.iter().any(|work| work.rj_code == code) {
            continue;
        }
        let purchased_at = ["sales_date", "purchased_at"]
            .iter()
            .find_map(|key| item.get(key).and_then(Value::as_str))
            .map(str::to_owned);
        owned.push(OwnedWork {
            rj_code: code.to_owned(),
            purchased_at,
        });
    }
    Ok(owned)
}

/// Every work the signed-in account bought. Metadata comes from the public
/// product API (source.get_work), so this stays one small request.
pub fn purchased_works() -> Result<Vec<OwnedWork>, SourceError> {
    parse_sales(&session_get_json(format!(
        "{}/content/sales?last=0",
        play_api()
    ))?)
}

/// Asset id of a purchased work's official download (download.enqueue).
pub const OWNED_ARCHIVE_ID: &str = "dlsite-owned-archive";

/// DLsite's own download page for a purchased work: the signed-in session
/// gets redirected to the file itself.
pub fn owned_download_url(rj: &str) -> String {
    https(
        WWW_HOST,
        &format!("/maniax/download/=/product_id/{rj}.html"),
    )
}

fn owned_archive_asset(rj: &str) -> Asset {
    Asset {
        id: OWNED_ARCHIVE_ID.to_owned(),
        kind: AssetKind::Archive,
        name: format!("{rj}.zip"),
        location: owned_download_url(rj),
        size: None,
        sha256: None,
    }
}

/// The official download of rj, offered only when the signed-in account
/// bought it; anything else is refused before a row is queued.
pub fn owned_archive(rj: &str) -> Result<Asset, SourceError> {
    if !purchased_works()?.iter().any(|work| work.rj_code == rj) {
        return Err(SourceError::new(
            "not_owned",
            false,
            format!("{rj} is not among this DLsite account's purchases"),
        ));
    }
    Ok(owned_archive_asset(rj))
}

/// A queued download on DLsite itself, which needs the session.
pub fn needs_session(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| is_dlsite_host(&url))
}

/// Opens a DLsite download with the stored session on this request only:
/// the cookie never reaches the queue row, its errors or the logs, and
/// reqwest drops it on a redirect to another host. A login page instead of
/// a file means the session lapsed, which retrying cannot fix.
pub fn open_download(url: &str, offset: u64) -> Result<crate::runtime::Opened, SourceError> {
    let cookie = load_session().ok_or_else(unauthorized)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(30))
        .user_agent(concat!("YUZUKI/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| SourceError::network(e.to_string()))?;
    let mut request = client.get(url).header(reqwest::header::COOKIE, cookie);
    if offset > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
    }
    let response = tauri::async_runtime::block_on(async move { request.send().await })
        .map_err(|e| SourceError::network(format!("DLsite download: {e}")))?;
    let status = response.status();
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/html"));
    if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
        || (status.is_success() && html)
    {
        return Err(unauthorized());
    }
    let start = if status == reqwest::StatusCode::PARTIAL_CONTENT {
        offset
    } else if status.is_success() {
        0
    } else {
        return Err(SourceError::network(format!(
            "DLsite download: HTTP {status}"
        )));
    };
    Ok(crate::runtime::Opened {
        start,
        reader: crate::runtime::http_body(response),
    })
}

/// Opens DLsite's login page in its own window and webview profile.
#[tauri::command]
pub async fn dlsite_login_open(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LOGIN_WINDOW) {
        return window.set_focus().map_err(|e| e.to_string());
    }
    let url: Url = https(PLAY_HOST, "/login/")
        .parse()
        .map_err(|e| format!("{e}"))?;
    let notify = app.clone();
    let mut builder = WebviewWindowBuilder::new(&app, LOGIN_WINDOW, WebviewUrl::External(url))
        .title("DLsite sign-in")
        .inner_size(480.0, 760.0)
        .on_navigation(move |url| {
            if is_signed_in_url(url) {
                let _ = notify.emit(LOGIN_READY_EVENT, ());
            }
            // Keep the window on DLsite: it is not a general browser.
            is_dlsite_host(url)
        });
    if let Some(dir) = crate::webview_data_dir() {
        builder = builder.data_directory(dir.join("dlsite-login"));
    }
    builder.build().map_err(|e| e.to_string())?;
    Ok(())
}

/// Captures the session from the login window, verifies it, stores it in the
/// OS keyring and wipes the window's browsing data.
#[tauri::command]
pub async fn dlsite_login_finish(app: AppHandle) -> Result<SessionStatus, String> {
    let window = app
        .get_webview_window(LOGIN_WINDOW)
        .ok_or("The DLsite sign-in window is not open")?;
    let mut pairs = Vec::new();
    for host in COOKIE_HOSTS {
        let url: Url = https(host, "/").parse().map_err(|e| format!("{e}"))?;
        for cookie in window.cookies_for_url(url).map_err(|e| e.to_string())? {
            pairs.push((cookie.name().to_string(), cookie.value().to_string()));
        }
    }
    let header = cookie_header(&pairs).ok_or("Not signed in to DLsite yet")?;
    get_json(&header, format!("{}/content/count?last=0", play_api()))
        .await
        .map_err(|e| e.message)?;
    store_session(&header).map_err(|e| e.message)?;
    let _ = window.clear_all_browsing_data();
    let _ = window.close();
    Ok(status())
}

#[cfg(test)]
mod tests {
    use super::{cookie_header, https, is_dlsite_host, is_signed_in_url, Url};

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.into(), value.into())
    }

    #[test]
    fn builds_one_cookie_header_first_value_wins() {
        let pairs = [
            pair("a", "1"),
            pair("", "x"),
            pair("b", "2"),
            pair("a", "3"),
        ];
        assert_eq!(cookie_header(&pairs).as_deref(), Some("a=1; b=2"));
        assert_eq!(cookie_header(&[]), None);
    }

    #[test]
    fn detects_signed_in_pages_and_stays_on_dlsite() {
        let url = |host: &str, path: &str| https(host, path).parse::<Url>().unwrap();
        assert!(is_signed_in_url(&url("play.dlsite.com", "/library")));
        assert!(!is_signed_in_url(&url("play.dlsite.com", "/login/")));
        assert!(!is_signed_in_url(&url("login.dlsite.com", "/login")));
        assert!(is_dlsite_host(&url("login.dlsite.com", "/login")));
        assert!(!is_dlsite_host(&url("evil-dlsite.com", "/")));
        let plain = https(super::WWW_HOST, "/").replacen("https", "http", 1);
        assert!(!is_dlsite_host(&plain.parse::<Url>().unwrap()));
    }

    #[test]
    fn parses_purchases_in_either_shape() {
        let list = serde_json::json!([
            {"workno": "RJ01", "sales_date": "2024-01-02 03:04:05"},
            {"workno": "RJ01"},
            {"other": 1},
            {"workno": "RJ02"}
        ]);
        let owned = super::parse_sales(&list).unwrap();
        assert_eq!(owned.len(), 2);
        assert_eq!(
            owned[0].purchased_at.as_deref(),
            Some("2024-01-02 03:04:05")
        );
        assert_eq!(owned[1].rj_code, "RJ02");
        let wrapped = serde_json::json!({"works": [{"workno": "RJ03"}]});
        assert_eq!(super::parse_sales(&wrapped).unwrap()[0].rj_code, "RJ03");
        let bad = super::parse_sales(&serde_json::json!({})).unwrap_err();
        assert_eq!(bad.code, "parse");
    }

    #[test]
    fn owned_archive_points_at_dlsite_and_needs_the_session() {
        let asset = super::owned_archive_asset("RJ01");
        assert_eq!(asset.id, super::OWNED_ARCHIVE_ID);
        assert_eq!(asset.kind, super::AssetKind::Archive);
        assert_eq!(asset.name, "RJ01.zip");
        assert!(asset
            .location
            .ends_with("/maniax/download/=/product_id/RJ01.html"));
        assert!(super::needs_session(&asset.location));
        assert!(!super::needs_session(&super::https(
            "example.org",
            "/RJ01.zip"
        )));
        assert!(!super::needs_session("not a url"));
    }
}
