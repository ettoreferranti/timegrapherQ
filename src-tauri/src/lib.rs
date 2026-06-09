//! TimegrapherQ desktop application shell.
//!
//! The heavy lifting (DSP / measurement) lives in the `timegrapherq-core`
//! crate. This module wires that core to the UI via Tauri commands and owns
//! audio I/O and storage as those land in later milestones.

use serde::Serialize;

#[derive(Serialize)]
struct Health {
    app_version: String,
    core_version: String,
}

/// Lightweight health check so the frontend can confirm the Rust backend and
/// the DSP core are reachable. Expanded with real capabilities in M1+.
#[tauri::command]
fn health() -> Health {
    Health {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        core_version: timegrapherq_core::VERSION.to_string(),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![health])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
