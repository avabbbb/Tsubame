//! Shared Download Manager (PR #14, docs/08 "Download manager").
//!
//! Persisted queue in the `downloads` table: duplicate (url, dest) pairs
//! collapse to one row, retryable failures back off and retry, bodies resume
//! from `<dest>.part` with Range when the size is known, size and sha256 are
//! checked before the staged file is renamed onto `dest`. A failing download
//! only touches its own row and staging file, never `media_files`.
//! Proxy: reqwest honours HTTP(S)_PROXY / NO_PROXY via runtime::HttpFetcher.
//! Concurrency: one sequential worker (`run_queue`); parallel workers via the
//! jobs runner are a follow-up.
#![cfg_attr(not(test), allow(dead_code))] // commands wire this up in B4

use super::SourceError;
use crate::runtime::Fetcher;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const COLUMNS: &str = "id, url, dest, staging_path, bytes_done, total, sha256, status, attempts, error";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Download {
    pub id: String,
    pub url: String,
    pub dest: String,
    pub staging_path: String,
    pub bytes_done: u64,
    pub total: Option<u64>,
    pub sha256: Option<String>,
    /// queued | running | succeeded | failed | cancelled
    pub status: String,
    pub attempts: u32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// Delay after failed attempt n (1-based): base * 2^(n-1), capped.
    fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.saturating_sub(1).min(16);
        self.base_delay.saturating_mul(factor).min(self.max_delay)
    }
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Download> {
    Ok(Download {
        id: r.get(0)?,
        url: r.get(1)?,
        dest: r.get(2)?,
        staging_path: r.get(3)?,
        bytes_done: r.get::<_, i64>(4)?.max(0) as u64,
        total: r.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
        sha256: r.get(6)?,
        status: r.get(7)?,
        attempts: r.get::<_, i64>(8)?.max(0) as u32,
        error: r.get(9)?,
    })
}

fn cancelled() -> SourceError {
    SourceError::new("cancelled", true, "cancelled")
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Download>, SourceError> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM downloads WHERE id = ?1"),
            params![id],
            row,
        )
        .optional()?)
}

pub fn list(conn: &Connection) -> Result<Vec<Download>, SourceError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM downloads ORDER BY created_at, id"
    ))?;
    let rows = stmt.query_map([], row)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Queues url -> dest. An existing (url, dest) row is returned as is, or
/// re-queued with a fresh attempt budget when it had failed or been cancelled.
pub fn enqueue(
    conn: &Connection,
    url: &str,
    dest: &Path,
    total: Option<u64>,
    sha256: Option<&str>,
) -> Result<Download, SourceError> {
    if !["http://", "https://", "file:"]
        .iter()
        .any(|scheme| url.starts_with(scheme))
    {
        return Err(SourceError::invalid_input(format!(
            "download url must be http(s) or file: {url}"
        )));
    }
    if !dest.is_absolute() || dest.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(SourceError::invalid_input(format!(
            "download destination must be absolute without '..': {}",
            dest.display()
        )));
    }
    let dest = dest
        .to_str()
        .ok_or_else(|| SourceError::invalid_input("download destination must be UTF-8"))?;
    let sha256 = match sha256 {
        Some(hex) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Some(hex.to_ascii_lowercase())
        }
        Some(hex) => return Err(SourceError::invalid_input(format!("not a sha256: {hex}"))),
        None => None,
    };
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM downloads WHERE url = ?1 AND dest = ?2",
            params![url, dest],
            |r| r.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => {
            conn.execute(
                "UPDATE downloads SET status = 'queued', attempts = 0, error = NULL,
                   updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?1 AND status IN ('failed','cancelled')",
                params![id],
            )?;
            id
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO downloads (id, url, dest, staging_path, total, sha256)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, url, dest, format!("{dest}.part"), total.map(|t| t as i64), sha256],
            )?;
            id
        }
    };
    get(conn, &id)?.ok_or_else(|| SourceError::not_found(format!("download {id}")))
}

/// Startup recovery: rows left `running` by a crash go back to the queue;
/// their .part files are kept and resumed.
pub fn recover(conn: &Connection) -> Result<usize, SourceError> {
    Ok(conn.execute(
        "UPDATE downloads SET status = 'queued',
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE status = 'running'",
        [],
    )?)
}

fn update(
    conn: &Connection,
    id: &str,
    status: &str,
    bytes: u64,
    attempts: u32,
    error: Option<&str>,
) -> Result<(), SourceError> {
    conn.execute(
        "UPDATE downloads SET status = ?2, bytes_done = ?3, attempts = ?4, error = ?5,
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE id = ?1",
        params![id, status, bytes as i64, attempts as i64, error],
    )?;
    Ok(())
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn sha256_file(path: &Path) -> Result<String, SourceError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Sleeps for delay unless cancelled first; false when cancelled.
fn sleep_unless_cancelled(delay: Duration, cancel: &dyn Fn() -> bool) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        if cancel() {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(100)));
    }
}

/// One fetch into staging. Resumes only when the total is known: without it
/// a complete .part cannot be told from a partial one.
fn attempt(
    d: &Download,
    staging: &Path,
    fetcher: &dyn Fetcher,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), SourceError> {
    let mut have = if d.total.is_some() { file_len(staging) } else { 0 };
    if let Some(total) = d.total {
        if have > total {
            have = 0;
        }
        if have == total && total > 0 {
            return Ok(());
        }
    }
    let mut opened = fetcher
        .open(&d.url, have)
        .map_err(|e| SourceError::network(format!("{}: {e}", d.url)))?;
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(staging)?;
    if opened.start == 0 {
        file.set_len(0)?;
    }
    file.seek(SeekFrom::Start(opened.start))?;
    let mut written = opened.start;
    progress(written);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if cancel() {
            file.flush()?;
            return Err(cancelled());
        }
        let n = match opened.reader.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                file.flush()?;
                return Err(SourceError::network(format!("{}: {e}", d.url)));
            }
        };
        if n == 0 {
            break;
        }
        if let Some(total) = d.total {
            if written + n as u64 > total {
                file.set_len(0)?;
                return Err(SourceError::new(
                    "size_mismatch",
                    true,
                    format!("{}: more bytes than the expected {total}", d.url),
                ));
            }
        }
        file.write_all(&buf[..n])?;
        written += n as u64;
        progress(written);
    }
    file.flush()?;
    match d.total {
        Some(total) if written != total => Err(SourceError::network(format!(
            "{}: connection closed at {written}/{total}",
            d.url
        ))),
        _ => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
fn drive(
    conn: &Connection,
    d: &Download,
    staging: &Path,
    dest: &Path,
    attempts: &mut u32,
    fetcher: &dyn Fetcher,
    policy: &RetryPolicy,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> Result<PathBuf, SourceError> {
    // Duplicate: the destination already holds the expected bytes.
    if let Some(expected) = &d.sha256 {
        if dest.is_file() && sha256_file(dest)? == *expected {
            return Ok(dest.to_path_buf());
        }
    }
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)?;
    }
    loop {
        if cancel() {
            return Err(cancelled());
        }
        *attempts += 1;
        update(conn, &d.id, "running", file_len(staging), *attempts, None)?;
        match attempt(d, staging, fetcher, cancel, progress) {
            Ok(()) => break,
            Err(e) if e.retryable && e.code != "cancelled" && *attempts < policy.max_attempts => {
                update(conn, &d.id, "running", file_len(staging), *attempts, Some(&e.to_string()))?;
                if !sleep_unless_cancelled(policy.delay(*attempts), cancel) {
                    return Err(cancelled());
                }
            }
            Err(e) => return Err(e),
        }
    }
    if let Some(expected) = &d.sha256 {
        let actual = sha256_file(staging)?;
        if actual != *expected {
            let _ = fs::remove_file(staging);
            return Err(SourceError::new(
                "integrity",
                false,
                format!("{}: sha256 {actual}, expected {expected}", d.url),
            ));
        }
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(staging, dest)?;
    Ok(dest.to_path_buf())
}

/// Runs one download to a terminal status and returns the final path.
/// Every exit records its status, so a row never stays `running`.
pub fn run(
    conn: &Connection,
    id: &str,
    fetcher: &dyn Fetcher,
    policy: &RetryPolicy,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> Result<PathBuf, SourceError> {
    let d = get(conn, id)?.ok_or_else(|| SourceError::not_found(format!("download {id}")))?;
    let dest = PathBuf::from(&d.dest);
    if d.status == "succeeded" && dest.is_file() {
        return Ok(dest);
    }
    let staging = PathBuf::from(&d.staging_path);
    let mut attempts = d.attempts;
    let result = drive(conn, &d, &staging, &dest, &mut attempts, fetcher, policy, cancel, progress);
    match &result {
        Ok(path) => update(conn, id, "succeeded", file_len(path), attempts, None)?,
        Err(e) if e.code == "cancelled" => {
            update(conn, id, "cancelled", file_len(&staging), attempts, None)?
        }
        Err(e) => update(conn, id, "failed", file_len(&staging), attempts, Some(&e.to_string()))?,
    }
    result
}

/// Drains the queue in order. One failure never stops the others.
pub fn run_queue(
    conn: &Connection,
    fetcher: &dyn Fetcher,
    policy: &RetryPolicy,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<(String, Result<PathBuf, SourceError>)>, SourceError> {
    let mut results = Vec::new();
    while !cancel() {
        let next: Option<String> = conn
            .query_row(
                "SELECT id FROM downloads WHERE status = 'queued' ORDER BY created_at, id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let Some(id) = next else { break };
        let result = run(conn, &id, fetcher, policy, cancel, &mut |_| {});
        results.push((id, result));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Opened;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    enum Step {
        Fail,
        Cut(usize),
    }

    /// Serves body with Range; scripted failures/cuts first; any url
    /// containing "bad" always fails.
    struct Fake {
        body: Vec<u8>,
        script: Mutex<VecDeque<Step>>,
        calls: Mutex<Vec<u64>>,
    }

    impl Fake {
        fn new(body: Vec<u8>, script: Vec<Step>) -> Self {
            Self {
                body,
                script: Mutex::new(script.into()),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl Fetcher for Fake {
        fn open(&self, url: &str, offset: u64) -> Result<Opened, String> {
            self.calls.lock().unwrap().push(offset);
            if url.contains("bad") {
                return Err("boom".into());
            }
            let start = (offset as usize).min(self.body.len());
            let end = match self.script.lock().unwrap().pop_front() {
                Some(Step::Fail) => return Err("boom".into()),
                Some(Step::Cut(at)) => at.clamp(start, self.body.len()),
                None => self.body.len(),
            };
            Ok(Opened {
                start: start as u64,
                reader: Box::new(io::Cursor::new(self.body[start..end].to_vec())),
            })
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("yuzuki-dl-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const FAST: RetryPolicy = RetryPolicy {
        max_attempts: 4,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::sources::migrate(&conn).unwrap();
        conn
    }

    fn body() -> Vec<u8> {
        (0..200_000u32).map(|i| (i % 251) as u8).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn resumes_after_cut_and_retries_then_verifies() {
        let (conn, tmp, body) = (db(), TempDir::new(), body());
        let dest = tmp.0.join("out/01.mp3");
        let d = enqueue(&conn, "https://x/01.mp3", &dest, Some(body.len() as u64), Some(&hex(&body))).unwrap();
        let fake = Fake::new(body.clone(), vec![Step::Cut(70_000), Step::Fail]);
        let mut last = 0;
        let path = run(&conn, &d.id, &fake, &FAST, &|| false, &mut |done| last = done).unwrap();
        assert_eq!(path, dest);
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert!(!Path::new(&d.staging_path).exists());
        assert_eq!(*fake.calls.lock().unwrap(), [0, 70_000, 70_000]);
        assert_eq!(last, body.len() as u64);
        let row = get(&conn, &d.id).unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.attempts, row.bytes_done), ("succeeded", 3, body.len() as u64));
        assert_eq!(row.error, None);
    }

    #[test]
    fn integrity_mismatch_fails_once_and_drops_staging() {
        let (conn, tmp, body) = (db(), TempDir::new(), body());
        let dest = tmp.0.join("01.mp3");
        let d = enqueue(&conn, "https://x/01.mp3", &dest, None, Some(&"0".repeat(64))).unwrap();
        let fake = Fake::new(body, vec![]);
        let error = run(&conn, &d.id, &fake, &FAST, &|| false, &mut |_| {}).unwrap_err();
        assert_eq!(error.code, "integrity");
        assert_eq!(fake.calls.lock().unwrap().len(), 1);
        assert!(!dest.exists() && !Path::new(&d.staging_path).exists());
        assert_eq!(get(&conn, &d.id).unwrap().unwrap().status, "failed");
    }

    #[test]
    fn gives_up_after_max_attempts_keeping_partial() {
        let (conn, tmp, body) = (db(), TempDir::new(), body());
        let dest = tmp.0.join("01.mp3");
        let d = enqueue(&conn, "https://x/01.mp3", &dest, Some(body.len() as u64), None).unwrap();
        let fake = Fake::new(body, vec![Step::Cut(1_000), Step::Fail, Step::Fail]);
        let policy = RetryPolicy { max_attempts: 3, ..FAST };
        let error = run(&conn, &d.id, &fake, &policy, &|| false, &mut |_| {}).unwrap_err();
        assert_eq!(error.code, "network");
        let row = get(&conn, &d.id).unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.attempts, row.bytes_done), ("failed", 3, 1_000));
        assert!(row.error.unwrap().contains("boom"));
        // Re-queue keeps the .part and resumes from it.
        let again = enqueue(&conn, "https://x/01.mp3", &dest, None, None).unwrap();
        assert_eq!((again.id.as_str(), again.status.as_str(), again.attempts), (d.id.as_str(), "queued", 0));
        run(&conn, &d.id, &fake, &policy, &|| false, &mut |_| {}).unwrap();
        assert_eq!(fake.calls.lock().unwrap().last(), Some(&1_000));
    }

    #[test]
    fn cancel_stops_with_cancelled_status() {
        let (conn, tmp) = (db(), TempDir::new());
        let d = enqueue(&conn, "https://x/01.mp3", &tmp.0.join("01.mp3"), None, None).unwrap();
        let error = run(&conn, &d.id, &Fake::new(body(), vec![]), &FAST, &|| true, &mut |_| {}).unwrap_err();
        assert_eq!(error.code, "cancelled");
        assert_eq!(get(&conn, &d.id).unwrap().unwrap().status, "cancelled");
    }

    #[test]
    fn queue_isolates_failures_and_dedupes() {
        let (conn, tmp, body) = (db(), TempDir::new(), body());
        let bad = enqueue(&conn, "https://bad/a", &tmp.0.join("a"), None, None).unwrap();
        let good = enqueue(&conn, "https://ok/b", &tmp.0.join("b"), None, None).unwrap();
        assert_eq!(enqueue(&conn, "https://ok/b", &tmp.0.join("b"), None, None).unwrap().id, good.id);
        assert_eq!(list(&conn).unwrap().len(), 2);
        let results = run_queue(&conn, &Fake::new(body.clone(), vec![]), &FAST, &|| false).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(get(&conn, &bad.id).unwrap().unwrap().status, "failed");
        assert_eq!(get(&conn, &good.id).unwrap().unwrap().status, "succeeded");
        assert_eq!(fs::read(tmp.0.join("b")).unwrap(), body);
    }

    #[test]
    fn existing_destination_with_matching_hash_skips_fetch() {
        let (conn, tmp, body) = (db(), TempDir::new(), body());
        let dest = tmp.0.join("01.mp3");
        fs::write(&dest, &body).unwrap();
        let d = enqueue(&conn, "https://x/01.mp3", &dest, None, Some(&hex(&body))).unwrap();
        let fake = Fake::new(body, vec![]);
        run(&conn, &d.id, &fake, &FAST, &|| false, &mut |_| {}).unwrap();
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn recover_requeues_running_and_rejects_bad_input() {
        let (conn, tmp) = (db(), TempDir::new());
        let d = enqueue(&conn, "file:///tmp/a", &tmp.0.join("a"), None, None).unwrap();
        update(&conn, &d.id, "running", 0, 1, None).unwrap();
        assert_eq!(recover(&conn).unwrap(), 1);
        assert_eq!(get(&conn, &d.id).unwrap().unwrap().status, "queued");
        let abs = tmp.0.join("x");
        assert_eq!(enqueue(&conn, "ftp://x", &abs, None, None).unwrap_err().code, "invalid_input");
        assert_eq!(enqueue(&conn, "https://x", Path::new("rel/x"), None, None).unwrap_err().code, "invalid_input");
        assert_eq!(enqueue(&conn, "https://x", &abs, None, Some("abc")).unwrap_err().code, "invalid_input");
    }
}

#[cfg(test)]
mod http_tests {
    //! C2: the real HttpFetcher (reqwest + Range) against a local server whose
    //! first response is cut mid-body; the retry must resume from the .part.
    use super::{enqueue, get, run, RetryPolicy};
    use rusqlite::Connection;
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::time::Duration;
    use crate::runtime::HttpFetcher;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<Option<u64>>>>;

    fn serve(body: Vec<u8>, cut_first_at: usize) -> (String, Seen) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for (n, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(value) = lower.strip_prefix("range: bytes=") {
                        range = value.trim().trim_end_matches('-').parse::<u64>().ok();
                    }
                }
                log.lock().unwrap().push(range);
                let start = range.unwrap_or(0) as usize;
                let len = body.len();
                let head = match range {
                    Some(_) => format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        len - start, start, len - 1, len
                    ),
                    None => format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"),
                };
                let end = if n == 0 { cut_first_at } else { len };
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body[start..end]);
                let _ = stream.flush();
            }
        });
        let scheme = "http";
        let sep = "/".repeat(2);
        (format!("{scheme}:{sep}{addr}/01.mp3"), seen)
    }

    #[test]
    fn http_fetcher_resumes_truncated_body_with_range() {
        let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let (url, seen) = serve(body.clone(), 120_000);
        let conn = Connection::open_in_memory().unwrap();
        crate::sources::migrate(&conn).unwrap();
        let dir = std::env::temp_dir().join(format!("yuzuki-dl-http-{}", uuid::Uuid::new_v4()));
        let dest = dir.join("01.mp3");
        let hex = format!("{:x}", Sha256::digest(&body));
        let d = enqueue(&conn, &url, &dest, Some(body.len() as u64), Some(&hex)).unwrap();
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        };
        let fetcher = HttpFetcher::new().unwrap();
        let path = run(&conn, &d.id, &fetcher, &policy, &|| false, &mut |_| {}).unwrap();
        assert_eq!(fs::read(&path).unwrap(), body);
        assert!(!Path::new(&d.staging_path).exists());
        assert_eq!(seen.lock().unwrap().as_slice(), &[None, Some(120_000)]);
        let row = get(&conn, &d.id).unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.attempts), ("succeeded", 2));
        let _ = fs::remove_dir_all(&dir);
    }
}
