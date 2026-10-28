//! Data root: where the library DB, runtime packs, models, caches and TTS
//! assets live. The default is project-local, never the per-user profile on
//! the system drive:
//! - YUZUKI_DATA_DIR when set (explicit override);
//! - debug builds: <repo>/.yuzuki-data (gitignored);
//! - release builds: <exe dir>/data when that directory is writable;
//! - otherwise the platform app data directory (e.g. an install under
//!   Program Files, or a signed macOS bundle).
//!
//! On the first start in a new root, the legacy app-data library is copied
//! over once and left in place. control.json stays in app data so the CLI and
//! MCP keep finding the Desktop.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

pub const ENV_DATA_DIR: &str = "YUZUKI_DATA_DIR";

/// Files and directories copied from the legacy app data directory.
const LEGACY_ITEMS: &[&str] = &[
    "tsubame.db",
    "tsubame.db-wal",
    "tsubame.db-shm",
    "dictionary.db",
    "runtime",
    "runtimes",
    "assets",
];

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Picks the data root, copies the legacy library into it once, and records
/// it for get. Called once from app setup.
pub fn init(app_data: Option<&Path>) -> Result<PathBuf, String> {
    let explicit = std::env::var_os(ENV_DATA_DIR)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty());
    let root = choose(explicit, default_dir(), app_data)?;
    if let Some(legacy) = app_data {
        if let Err(error) = migrate_legacy(legacy, &root) {
            eprintln!("data root: legacy library copy failed: {error}");
        }
    }
    eprintln!("data root: {}", root.display());
    let _ = ROOT.set(root.clone());
    Ok(root)
}

/// The data root chosen at startup; None before init (unit tests).
pub fn get() -> Option<&'static Path> {
    ROOT.get().map(PathBuf::as_path)
}

#[cfg(debug_assertions)]
fn default_dir() -> Option<PathBuf> {
    Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.yuzuki-data"))
}

#[cfg(not(debug_assertions))]
fn default_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("data"))
}

fn choose(
    explicit: Option<PathBuf>,
    project: Option<PathBuf>,
    app_data: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(dir) = explicit {
        let dir = normalize(&dir);
        ensure_writable(&dir)
            .map_err(|e| format!("{ENV_DATA_DIR}={} is not writable: {e}", dir.display()))?;
        return Ok(dir);
    }
    if let Some(dir) = project.map(|dir| normalize(&dir)) {
        match ensure_writable(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) => eprintln!(
                "data root: {} is not writable ({e}); using the app data directory",
                dir.display()
            ),
        }
    }
    let dir = app_data
        .ok_or_else(|| "no writable data directory".to_string())?
        .to_path_buf();
    ensure_writable(&dir)
        .map_err(|e| format!("app data directory {} is not writable: {e}", dir.display()))?;
    Ok(dir)
}

fn ensure_writable(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let probe = dir.join(".write-probe");
    fs::write(&probe, b"")?;
    fs::remove_file(probe)
}

/// Drops . and folds .. without touching the disk (canonicalize would
/// return \\?\ paths on Windows, which the asset scope and Python dislike).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Copies the legacy library once: only into a different root that has no
/// library DB yet. The legacy copy is left untouched.
fn migrate_legacy(legacy: &Path, root: &Path) -> std::io::Result<Vec<String>> {
    if normalize(legacy) == normalize(root)
        || root.join("tsubame.db").exists()
        || !legacy.join("tsubame.db").exists()
    {
        return Ok(Vec::new());
    }
    let mut copied = Vec::new();
    for name in LEGACY_ITEMS {
        let src = legacy.join(name);
        if src.is_file() {
            fs::copy(&src, root.join(name))?;
        } else if src.is_dir() {
            copy_dir(&src, &root.join(name))?;
        } else {
            continue;
        }
        copied.push((name).to_string());
    }
    eprintln!("data root: copied {copied:?} from {}", legacy.display());
    Ok(copied)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{choose, fs, migrate_legacy, normalize, Path, PathBuf};

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("yuzuki-data-root-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn normalize_folds_parent_components() {
        let path = Path::new("repo")
            .join("app")
            .join("src-tauri")
            .join("..")
            .join("..")
            .join(".yuzuki-data");
        assert_eq!(normalize(&path), Path::new("repo").join(".yuzuki-data"));
    }

    #[test]
    fn explicit_then_project_then_app_data() {
        let (explicit, project, app) = (temp("explicit"), temp("project"), temp("app"));
        let pick = |e: Option<&PathBuf>, p: Option<&PathBuf>| {
            choose(e.cloned(), p.cloned(), Some(app.as_path())).unwrap()
        };
        assert_eq!(pick(Some(&explicit), Some(&project)), explicit);
        assert_eq!(pick(None, Some(&project)), project);
        assert_eq!(pick(None, None), app);
        for dir in [explicit, project, app] {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn legacy_library_is_copied_once_and_left_in_place() {
        let (legacy, root) = (temp("legacy"), temp("root"));
        fs::create_dir_all(legacy.join("runtime").join("packs")).unwrap();
        fs::write(legacy.join("tsubame.db"), b"db").unwrap();
        fs::write(legacy.join("runtime").join("packs").join("x"), b"p").unwrap();
        fs::create_dir_all(&root).unwrap();

        let copied = migrate_legacy(&legacy, &root).unwrap();
        assert_eq!(copied, vec!["tsubame.db".to_string(), "runtime".to_string()]);
        assert_eq!(fs::read(root.join("runtime").join("packs").join("x")).unwrap(), b"p");
        assert!(legacy.join("tsubame.db").exists());

        fs::write(root.join("tsubame.db"), b"new").unwrap();
        assert!(migrate_legacy(&legacy, &root).unwrap().is_empty());
        assert_eq!(fs::read(root.join("tsubame.db")).unwrap(), b"new");
        for dir in [legacy, root] {
            let _ = fs::remove_dir_all(dir);
        }
    }
}
