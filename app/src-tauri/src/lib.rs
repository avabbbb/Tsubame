mod db;
mod subtitle;

use db::{MediaDb, MediaItem, Segment};
use std::{fs, sync::Mutex};
use subtitle::{export_subtitles, parse_subtitles, SubtitleFormat};
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
fn update_segment(
    id: i64,
    expected_revision: i64,
    source_text: String,
    translated_text: String,
    start_ms: i64,
    end_ms: i64,
    state: State<AppState>,
) -> Result<Segment, String> {
    if end_ms <= start_ms || start_ms < 0 {
        return Err("invalid segment timing".into());
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
    .ok_or_else(|| "segment revision conflict or invalid timing".to_string())
}

#[tauri::command]
fn import_subtitles(
    media_id: i64,
    path: String,
    target: String,
    state: State<AppState>,
) -> Result<Vec<Segment>, String> {
    let format = SubtitleFormat::from_path(&path)?;
    let contents = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let drafts = parse_subtitles(&contents, format)?;
    let provenance = format!("subtitle:{path}");

    let db = state.db.lock().map_err(|e| e.to_string())?;
    match target.as_str() {
        "source" => db
            .replace_source_segments(media_id, &drafts, &provenance)
            .map_err(|e| e.to_string()),
        "translation" => db
            .import_translation_segments(media_id, &drafts, &provenance)
            .map_err(|e| e.to_string()),
        _ => Err("target must be source or translation".into()),
    }
}

#[tauri::command]
fn export_subtitles_command(
    media_id: i64,
    path: String,
    target: String,
    state: State<AppState>,
) -> Result<(), String> {
    let format = SubtitleFormat::from_path(&path)?;
    let segments = state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .get_segments(media_id)
        .map_err(|e| e.to_string())?;

    let rows = segments
        .into_iter()
        .map(|segment| {
            let text = match target.as_str() {
                "source" => segment.source_text,
                "translation" => segment.translated_text,
                "bilingual" => {
                    if segment.translated_text.trim().is_empty() {
                        segment.source_text
                    } else {
                        format!("{}\n{}", segment.source_text, segment.translated_text)
                    }
                }
                _ => String::new(),
            };
            (segment.start_ms, segment.end_ms, text)
        })
        .filter(|(_, _, text)| !text.trim().is_empty())
        .collect::<Vec<_>>();

    if rows.is_empty() {
        return Err("no subtitle text to export".into());
    }

    let contents = export_subtitles(&rows, format);
    fs::write(path, contents).map_err(|e| e.to_string())
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
            update_segment,
            import_subtitles,
            export_subtitles_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tsubame");
}
