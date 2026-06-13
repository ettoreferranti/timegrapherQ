//! TimegrapherQ desktop application shell.
//!
//! The heavy lifting (DSP / measurement) lives in the `timegrapherq-core`
//! crate; audio capture lives in [`audio`]; the watch collection and history
//! live in [`db`]. This module exposes them to the UI as Tauri commands.

mod audio;
mod db;
mod import;
mod live;
mod monitor;

use std::path::Path;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Manager, State};

/// Shared application state: the open store plus settings.
struct AppState {
    store: db::Store,
    settings: db::Settings,
    config_dir: std::path::PathBuf,
    /// The running live session, if any.
    live: Option<live::SessionHandle>,
    /// The running mic-monitor session, if any.
    monitor: Option<monitor::MonitorHandle>,
}

type SharedState = Mutex<AppState>;

fn lock(state: &SharedState) -> Result<std::sync::MutexGuard<'_, AppState>, String> {
    state
        .lock()
        .map_err(|_| "application state is poisoned".to_string())
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

/// Analyse an audio file (WAV, M4A, MP3, FLAC, AIFF, OGG) as if it had been
/// recorded live — phone clips pressed against the watch work well.
#[tauri::command]
async fn analyze_file(
    path: String,
    bph: u32,
    lift_angle_deg: f64,
) -> Result<audio::MeasurementDto, String> {
    tauri::async_runtime::spawn_blocking(move || import::analyze_file(&path, bph, lift_angle_deg))
        .await
        .map_err(|e| format!("analysis task failed: {e}"))?
}

/// Start live mode: continuous capture + analysis, streamed to the UI as
/// `live-beats` / `live-metrics` events. Any previous session is stopped.
#[tauri::command]
async fn start_live(
    app: tauri::AppHandle,
    device_name: Option<String>,
    bph: u32,
    lift_angle_deg: f64,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    // Opening the device can block for a moment; keep it off the event loop.
    let handle = tauri::async_runtime::spawn_blocking(move || {
        live::start(app, device_name, bph, lift_angle_deg)
    })
    .await
    .map_err(|e| format!("live start failed: {e}"))??;
    let mut st = lock(&state)?;
    if let Some(prev) = st.live.take() {
        prev.stop();
    }
    // Live measurement and the mic monitor share the microphone.
    if let Some(mon) = st.monitor.take() {
        mon.stop();
    }
    st.live = Some(handle);
    Ok(())
}

/// Stop the live session, if one is running.
#[tauri::command]
fn stop_live(state: State<SharedState>) -> Result<(), String> {
    if let Some(session) = lock(&state)?.live.take() {
        session.stop();
    }
    Ok(())
}

/// Start the mic monitor: a fast input-level meter, plus audible passthrough
/// when `listen` is set. Any live session is stopped (shared microphone).
#[tauri::command]
async fn start_mic_monitor(
    app: tauri::AppHandle,
    device_name: Option<String>,
    listen: bool,
    gain: f64,
    iso_hz: f64,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let handle = tauri::async_runtime::spawn_blocking(move || {
        monitor::start(app, device_name, listen, gain as f32, iso_hz as f32)
    })
    .await
    .map_err(|e| format!("mic monitor start failed: {e}"))??;
    let mut st = lock(&state)?;
    if let Some(prev) = st.monitor.take() {
        prev.stop();
    }
    if let Some(live) = st.live.take() {
        live.stop();
    }
    st.monitor = Some(handle);
    Ok(())
}

/// Stop the mic monitor, if one is running.
#[tauri::command]
fn stop_mic_monitor(state: State<SharedState>) -> Result<(), String> {
    if let Some(mon) = lock(&state)?.monitor.take() {
        mon.stop();
    }
    Ok(())
}

/// Adjust the audible-passthrough gain of the running monitor (linear).
#[tauri::command]
fn set_monitor_gain(gain: f64, state: State<SharedState>) -> Result<(), String> {
    if let Some(mon) = &lock(&state)?.monitor {
        mon.set_gain(gain as f32);
    }
    Ok(())
}

/// Set the running monitor's isolation band-pass centre (Hz); 0 = broadband.
#[tauri::command]
fn set_monitor_iso(iso_hz: f64, state: State<SharedState>) -> Result<(), String> {
    if let Some(mon) = &lock(&state)?.monitor {
        mon.set_iso_hz(iso_hz as f32);
    }
    Ok(())
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
                live: None,
                monitor: None,
            }));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            health,
            list_input_devices,
            record_and_analyze,
            analyze_file,
            start_live,
            stop_live,
            start_mic_monitor,
            stop_mic_monitor,
            set_monitor_gain,
            set_monitor_iso,
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
