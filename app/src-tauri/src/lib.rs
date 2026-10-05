mod db;
mod subtitle;

use db::{MediaDb, MediaItem, Segment};
use std::{fs, path::Path, sync::Mutex};
use subtitle::{SubtitleCue, SubtitleFormat};
use tauri::{Manager, State};

struct AppState {
    db: Mutex<MediaDb>,
}

#[tauri::command]
fn list_media(state: State<AppState>) -> Result<Vec<MediaItem>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .list_media()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn import_media(path: String, state: State<AppState>) -> Result<MediaItem, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .import_media(&path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_media_duration(id: i64, duration_ms: i64, state: State<AppState>) -> Result<(), String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .update_media_duration(id, duration_ms)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_playback_state(
    id: i64,
    position_ms: i64,
    speed: f64,
    volume: f64,
    state: State<AppState>,
) -> Result<(), String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .save_playback_state(id, position_ms, speed, volume)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_segments(media_id: i64, state: State<AppState>) -> Result<Vec<Segment>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .get_segments(media_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_segment(id: i64, state: State<AppState>) -> Result<Option<Segment>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .get_segment(id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_segment(
    id: i64,
    expected_revision: i64,
    source_text: String,
    translated_text: String,
    start_ms: i64,
    end_ms: i64,
    state: State<AppState>,
) -> Result<Segment, String> {
    if start_ms < 0 {
        return Err("segment start must be non-negative".to_string());
    }
    if end_ms <= start_ms {
        return Err("segment end must be after start".to_string());
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.update_segment(
        id,
        expected_revision,
        &source_text,
        &translated_text,
        start_ms,
        end_ms,
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "segment revision conflict".to_string())
}

#[tauri::command]
fn import_subtitles(
    media_id: i64,
    path: String,
    state: State<AppState>,
) -> Result<Vec<Segment>, String> {
    let path_ref = Path::new(&path);
    let format = SubtitleFormat::from_path(path_ref)?;
    let content = fs::read_to_string(path_ref).map_err(|e| e.to_string())?;
    let cues = subtitle::parse(&content, format)?;

    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .replace_segments(media_id, &cues, &format!("import:{}", format.label()))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn export_subtitles(
    media_id: i64,
    path: String,
    text_mode: String,
    state: State<AppState>,
) -> Result<String, String> {
    let path_ref = Path::new(&path);
    let format = SubtitleFormat::from_path(path_ref)?;
    let rows = state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .get_segments(media_id)
        .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        return Err("track contains no segments".to_string());
    }

    let cues = rows
        .iter()
        .map(|segment| {
            let text = match text_mode.as_str() {
                "translation" => {
                    if segment.translated_text.trim().is_empty() {
                        segment.source_text.clone()
                    } else {
                        segment.translated_text.clone()
                    }
                }
                "bilingual" => {
                    if segment.translated_text.trim().is_empty() {
                        segment.source_text.clone()
                    } else {
                        format!("{}\n{}", segment.source_text, segment.translated_text)
                    }
                }
                _ => segment.source_text.clone(),
            };
            SubtitleCue {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text,
            }
        })
        .collect::<Vec<_>>();

    let output = subtitle::serialize(&cues, format);
    fs::write(path_ref, output).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db = MediaDb::open(&dir.join("tsubame.db"))?;
            app.manage(AppState { db: Mutex::new(db) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_media,
            import_media,
            update_media_duration,
            save_playback_state,
            get_segments,
            get_segment,
            update_segment,
            import_subtitles,
            export_subtitles
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tsubame");
}
