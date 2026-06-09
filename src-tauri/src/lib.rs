//! TimegrapherQ desktop application shell.
//!
//! The heavy lifting (DSP / measurement) lives in the `timegrapherq-core`
//! crate; audio capture lives in [`audio`]. This module exposes them to the UI
//! as Tauri commands.

mod audio;

use serde::Serialize;

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
///
/// Runs on a blocking thread so the UI stays responsive during the recording.
#[tauri::command]
async fn record_and_analyze(
    device_name: Option<String>,
    bph: u32,
    lift_angle_deg: f64,
    seconds: f64,
) -> Result<audio::MeasurementDto, String> {
    tauri::async_runtime::spawn_blocking(move || {
        audio::record_and_analyze(device_name, bph, lift_angle_deg, seconds)
    })
    .await
    .map_err(|e| format!("recording task failed: {e}"))?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            health,
            list_input_devices,
            record_and_analyze
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
