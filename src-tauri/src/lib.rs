//! TimegrapherQ desktop application shell.
//!
//! The heavy lifting (DSP / measurement) lives in the `timegrapherq-core`
//! crate; audio capture lives in [`audio`]; the watch collection and history
//! live in [`db`]. This module exposes them to the UI as Tauri commands.

mod audio;
mod db;

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Manager, State};

/// Shared application state: the open store plus settings.
struct AppState {
    store: db::Store,
    settings: db::Settings,
    config_dir: std::path::PathBuf,
}

type SharedState = Mutex<AppState>;

fn lock(state: &SharedState) -> Result<std::sync::MutexGuard<'_, AppState>, String> {
    state.lock().map_err(|_| "application state is poisoned".to_string())
}

#[derive(Serialize)]
struct Health {
    app_version: String,
    core_version: String,
}

/// Lightweight health check so the frontend can confirm the Rust backend and
/// the DSP core are reachable.
#[tauri::command]
fn health() -> Health {
    Health {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        core_version: timegrapherq_core::VERSION.to_string(),
    }
}

/// List available microphone input devices.
#[tauri::command]
fn list_input_devices() -> Vec<audio::DeviceInfo> {
    audio::list_devices()
}

/// Record from the given device (or default) for `seconds`, then analyse.
#[tauri::command]
async fn record_and_analyze(
    device_name: Option<String>,
    bph: u32,
    lift_angle_deg: f64,
    seconds: f64,
    save_recording: bool,
) -> Result<audio::MeasurementDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        audio::record_and_analyze(device_name, bph, lift_angle_deg, seconds, save_recording)
    })
    .await
    .map_err(|e| format!("recording task failed: {e}"))?
}

// ---- Settings ----

#[tauri::command]
fn get_settings(state: State<SharedState>) -> Result<db::Settings, String> {
    Ok(lock(&state)?.settings.clone())
}

#[tauri::command]
fn set_data_dir(path: String, state: State<SharedState>) -> Result<db::Settings, String> {
    let store = db::Store::open(Path::new(&path))?;
    let mut st = lock(&state)?;
    st.store = store;
    st.settings.data_dir = path;
    st.settings.save(&st.config_dir)?;
    Ok(st.settings.clone())
}

#[tauri::command]
fn set_default_clip(seconds: f64, state: State<SharedState>) -> Result<db::Settings, String> {
    if !(1.0..=120.0).contains(&seconds) {
        return Err("default clip length must be between 1 and 120 seconds".to_string());
    }
    let mut st = lock(&state)?;
    st.settings.default_clip_seconds = seconds;
    st.settings.save(&st.config_dir)?;
    Ok(st.settings.clone())
}

// ---- Watches ----

#[tauri::command]
fn list_watches(state: State<SharedState>) -> Result<Vec<db::Watch>, String> {
    lock(&state)?.store.list_watches()
}

#[tauri::command]
fn create_watch(input: db::WatchInput, state: State<SharedState>) -> Result<db::Watch, String> {
    lock(&state)?.store.create_watch(&input)
}

#[tauri::command]
fn update_watch(
    id: String,
    input: db::WatchInput,
    state: State<SharedState>,
) -> Result<db::Watch, String> {
    lock(&state)?.store.update_watch(&id, &input)
}

#[tauri::command]
fn delete_watch(id: String, state: State<SharedState>) -> Result<(), String> {
    lock(&state)?.store.delete_watch(&id)
}

// ---- Tests ----

#[tauri::command]
fn list_tests(watch_id: String, state: State<SharedState>) -> Result<Vec<db::Test>, String> {
    lock(&state)?.store.list_tests(&watch_id)
}

#[tauri::command]
fn save_test(
    watch_id: String,
    input: db::TestInput,
    state: State<SharedState>,
) -> Result<db::Test, String> {
    lock(&state)?.store.create_test(&watch_id, &input)
}

#[tauri::command]
fn update_test(
    id: String,
    edit: db::TestEdit,
    state: State<SharedState>,
) -> Result<db::Test, String> {
    lock(&state)?.store.update_test(&id, &edit)
}

#[tauri::command]
fn delete_test(id: String, state: State<SharedState>) -> Result<(), String> {
    lock(&state)?.store.delete_test(&id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let default_data = app.path().app_data_dir()?;
            let settings = db::Settings::load_or_default(&config_dir, &default_data);
            let store = db::Store::open(Path::new(&settings.data_dir))
                .map_err(Box::<dyn std::error::Error>::from)?;
            app.manage(Mutex::new(AppState {
                store,
                settings,
                config_dir,
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            health,
            list_input_devices,
            record_and_analyze,
            get_settings,
            set_data_dir,
            set_default_clip,
            list_watches,
            create_watch,
            update_watch,
            delete_watch,
            list_tests,
            save_test,
            update_test,
            delete_test
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
