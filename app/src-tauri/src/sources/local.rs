//! Local files/folders adapter (PR #14, docs/08-source-adapters.md).
//!
//! `remote_id` is an absolute path. A folder is one work (every recognised
//! file under it, recursively); a single media file is a work of that file
//! plus its same-directory sidecars: subtitles named after it (`01.vtt`,
//! `01.mp3.lrc`) and scripts (`01.txt`, `台本.pdf`). Read-only: it never
//! writes `media_files`; importing into the library is a separate step.

use super::{Asset, AssetKind, Capabilities, SourceAdapter, SourceError, Work};
use std::path::{Component, Path, PathBuf};

/// Directory levels a folder scan descends (matches import_folder).
const MAX_DEPTH: usize = 16;

pub struct LocalAdapter;

/// Asset kind by extension; None for files the adapter ignores.
/// Audio/video must stay in sync with `MEDIA_EXTENSIONS` in lib.rs.
pub fn classify(path: &Path) -> Option<AssetKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "mp3" | "m4a" | "wav" | "flac" | "ogg" | "opus" | "aac" | "m4b" => AssetKind::Audio,
        "mp4" | "m4v" | "webm" | "mkv" | "mov" | "avi" => AssetKind::Video,
        "vtt" | "srt" | "lrc" | "ass" | "ssa" => AssetKind::Subtitle,
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" => AssetKind::Image,
        "zip" | "rar" | "7z" => AssetKind::Archive,
        "txt" | "pdf" | "md" | "doc" | "docx" | "rtf" => AssetKind::Text,
        "json" | "nfo" | "xml" | "cue" => AssetKind::Metadata,
        _ => return None,
    })
}

/// First standalone RJ code (RJ + 6 or 8 digits) in text, upper-cased.
pub fn rj_code(text: &str) -> Option<String> {
    // ASCII upper-casing keeps byte offsets, so slices stay on char boundaries.
    let upper = text.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut from = 0;
    while let Some(pos) = upper[from..].find("RJ") {
        let at = from + pos;
        let start = at + 2;
        let digits = bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        let glued = at > 0 && bytes[at - 1].is_ascii_alphanumeric();
        if !glued && (digits == 6 || digits == 8) {
            return Some(format!("RJ{}", &upper[start..start + digits]));
        }
        from = start;
    }
    None
}

fn validate(remote_id: &str) -> Result<PathBuf, SourceError> {
    let path = Path::new(remote_id);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SourceError::invalid_input(format!(
            "local path must be absolute without '..': {remote_id}"
        )));
    }
    if !path.exists() {
        return Err(SourceError::not_found(format!(
            "path does not exist: {remote_id}"
        )));
    }
    Ok(path.to_path_buf())
}

/// Recognised files under dir, sorted by path. Hidden entries and symlinks
/// are skipped, so a link cycle can never loop the scan.
fn scan(dir: &Path) -> Result<Vec<PathBuf>, SourceError> {
    let mut found = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = pending.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let file_type = entry.file_type()?;
            let path = entry.path();
            if file_type.is_dir() {
                if depth + 1 < MAX_DEPTH {
                    pending.push((path, depth + 1));
                }
            } else if file_type.is_file() && classify(&path).is_some() {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Subtitles and scripts beside media that belong to it: same stem
/// (`01.vtt`), full name plus extension (`01.mp3.lrc`), or a 台本 script.
pub fn sidecars(media: &Path) -> Result<Vec<PathBuf>, SourceError> {
    let Some(dir) = media.parent() else {
        return Ok(Vec::new());
    };
    let stem = media
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let name = media
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path == media || !matches!(classify(&path), Some(AssetKind::Subtitle | AssetKind::Text))
        {
            continue;
        }
        let Some(other) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if other == stem || other == name || other.contains("台本") {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// None for unrecognised or non-UTF-8 paths (the library stores UTF-8).
fn asset(path: &Path) -> Option<Asset> {
    let kind = classify(path)?;
    let location = path.to_str()?.to_string();
    Some(Asset {
        id: location.clone(),
        kind,
        name: path.file_name()?.to_string_lossy().into_owned(),
        location,
        size: std::fs::metadata(path).ok().map(|meta| meta.len()),
        sha256: None,
    })
}

impl SourceAdapter for LocalAdapter {
    fn id(&self) -> &'static str {
        "local"
    }

    fn label(&self) -> &'static str {
        "Local files"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            get_work: true,
            list_assets: true,
            resolve_asset: true,
            ..Capabilities::default()
        }
    }

    fn get_work(&self, remote_id: &str) -> Result<Work, SourceError> {
        let root = validate(remote_id)?;
        let (title, files) = if root.is_dir() {
            let title = root.file_name().map(|s| s.to_string_lossy().into_owned());
            (title, scan(&root)?)
        } else {
            if !matches!(classify(&root), Some(AssetKind::Audio | AssetKind::Video)) {
                return Err(SourceError::invalid_input(format!(
                    "not a folder or media file: {remote_id}"
                )));
            }
            let title = root.file_stem().map(|s| s.to_string_lossy().into_owned());
            let mut files = vec![root.clone()];
            files.extend(sidecars(&root)?);
            (title, files)
        };
        let title = title.unwrap_or_else(|| remote_id.to_string());
        let assets: Vec<Asset> = files.iter().filter_map(|path| asset(path)).collect();
        let cover = assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Image)
            .map(|asset| asset.location.clone());
        Ok(Work {
            source_id: self.id().into(),
            remote_id: remote_id.into(),
            rj_code: rj_code(&title).or_else(|| rj_code(remote_id)),
            title,
            circle: None,
            voice_actors: Vec::new(),
            languages: Vec::new(),
            tags: Vec::new(),
            cover,
            release_date: None,
            assets,
        })
    }

    fn list_assets(&self, remote_id: &str) -> Result<Vec<Asset>, SourceError> {
        Ok(self.get_work(remote_id)?.assets)
    }

    fn resolve_asset(&self, asset_id: &str) -> Result<Asset, SourceError> {
        let path = validate(asset_id)?;
        if !path.is_file() {
            return Err(SourceError::invalid_input(format!(
                "not a file: {asset_id}"
            )));
        }
        asset(&path).ok_or_else(|| {
            SourceError::invalid_input(format!("unrecognised file type: {asset_id}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("yuzuki-local-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    fn names(assets: &[Asset], kind: AssetKind) -> Vec<String> {
        assets
            .iter()
            .filter(|asset| asset.kind == kind)
            .map(|asset| asset.name.clone())
            .collect()
    }

    #[test]
    fn rj_code_needs_standalone_six_or_eight_digits() {
        assert_eq!(rj_code("[RJ239066] おやすみ"), Some("RJ239066".into()));
        assert_eq!(rj_code("rj01234567_work"), Some("RJ01234567".into()));
        assert_eq!(rj_code("RJ12345"), None);
        assert_eq!(rj_code("XRJ239066"), None);
        assert_eq!(rj_code("おやすみ"), None);
    }

    #[test]
    fn folder_work_lists_every_recognised_file_by_kind() {
        let tmp = TempDir::new();
        let root = tmp.0.join("RJ239066 おやすみ");
        for name in [
            "01.mp3",
            "01.vtt",
            "01.mp3.lrc",
            "台本.txt",
            "cover.jpg",
            "SE/02.wav",
            ".hidden.mp3",
            "notes.xyz",
        ] {
            touch(&root.join(name));
        }
        let work = LocalAdapter.get_work(root.to_str().unwrap()).unwrap();
        assert_eq!(work.source_id, "local");
        assert_eq!(work.title, "RJ239066 おやすみ");
        assert_eq!(work.rj_code.as_deref(), Some("RJ239066"));
        assert_eq!(names(&work.assets, AssetKind::Audio), ["01.mp3", "02.wav"]);
        assert_eq!(
            names(&work.assets, AssetKind::Subtitle),
            ["01.mp3.lrc", "01.vtt"]
        );
        assert_eq!(names(&work.assets, AssetKind::Text), ["台本.txt"]);
        assert_eq!(
            work.cover,
            Some(root.join("cover.jpg").to_str().unwrap().to_string())
        );
        assert_eq!(work.assets.len(), 6);
        assert!(work.assets.iter().all(|asset| asset.size == Some(1)));
    }

    #[test]
    fn file_work_takes_only_its_own_sidecars() {
        let tmp = TempDir::new();
        for name in [
            "01.mp3",
            "01.srt",
            "01.mp3.lrc",
            "02.mp3",
            "02.vtt",
            "台本.pdf",
        ] {
            touch(&tmp.0.join(name));
        }
        let media = tmp.0.join("01.mp3");
        let work = LocalAdapter.get_work(media.to_str().unwrap()).unwrap();
        assert_eq!(work.title, "01");
        let all: Vec<&str> = work
            .assets
            .iter()
            .map(|asset| asset.name.as_str())
            .collect();
        assert_eq!(all, ["01.mp3", "01.mp3.lrc", "01.srt", "台本.pdf"]);
    }

    #[test]
    fn bad_paths_fail_with_typed_errors() {
        let tmp = TempDir::new();
        assert_eq!(
            LocalAdapter.get_work("relative/dir").unwrap_err().code,
            "invalid_input"
        );
        let missing = tmp.0.join("missing");
        assert_eq!(
            LocalAdapter
                .get_work(missing.to_str().unwrap())
                .unwrap_err()
                .code,
            "not_found"
        );
        let text = tmp.0.join("a.txt");
        touch(&text);
        assert_eq!(
            LocalAdapter
                .get_work(text.to_str().unwrap())
                .unwrap_err()
                .code,
            "invalid_input"
        );
        assert_eq!(
            LocalAdapter
                .resolve_asset(text.to_str().unwrap())
                .unwrap()
                .kind,
            AssetKind::Text
        );
    }
}
