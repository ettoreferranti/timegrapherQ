// Typed wrappers around the Tauri commands. Field names use snake_case to
// match the Rust serde structs exactly.
import { invoke } from "@tauri-apps/api/core";

export interface DeviceInfo {
  name: string;
  is_default: boolean;
  default_sample_rate: number;
  channels: number;
}

export interface MeasurementDto {
  rate_s_per_day: number;
  /** ~95% confidence half-width on the rate (s/day). */
  rate_ci95_s_per_day: number;
  beat_error_ms: number;
  amplitude_deg: number | null;
  bph: number;
  lift_angle_deg: number;
  beats_detected: number;
  beats_used: number;
  beats_expected: number;
  periodicity: number;
  detected_bph: number | null;
  /** Band-pass centre (Hz) the analyzer locked onto. */
  band_center_hz: number;
  /** Seconds silenced as loud outliers (handling bumps, coughs). */
  masked_seconds: number;
  quality: number;
  sample_rate: number;
  device_name: string;
  clip_seconds: number;
  peak_level: number;
  rms_level: number;
  raw_onsets: number;
  degraded_input: boolean;
  measured: boolean;
  recording_path: string | null;
  warning: string | null;
}

export interface Settings {
  data_dir: string;
  default_clip_seconds: number;
}

export interface Watch {
  id: string;
  name: string;
  brand: string | null;
  model: string | null;
  calibre: string | null;
  reference: string | null;
  serial: string | null;
  lift_angle: number;
  bph: number;
  notes: string | null;
  created_at: string;
  updated_at: string;
}

export interface WatchInput {
  name: string;
  brand: string | null;
  model: string | null;
  calibre: string | null;
  reference: string | null;
  serial: string | null;
  lift_angle: number;
  bph: number;
  notes: string | null;
}

export interface Test {
  id: string;
  watch_id: string;
  measured_at: string;
  position: string | null;
  rate_s_per_day: number | null;
  /** ~95% confidence half-width on the rate (s/day), when known. */
  rate_ci95_s_per_day: number | null;
  beat_error_ms: number | null;
  amplitude_deg: number | null;
  bph_used: number;
  lift_angle_used: number;
  temperature_c: number | null;
  power_state: string | null;
  device_name: string | null;
  sample_rate_hz: number | null;
  clip_seconds: number | null;
  quality: number | null;
  beats_used: number | null;
  audio_path: string | null;
  notes: string | null;
  created_at: string;
}

export interface TestInput {
  position: string | null;
  rate_s_per_day: number | null;
  rate_ci95_s_per_day: number | null;
  beat_error_ms: number | null;
  amplitude_deg: number | null;
  bph_used: number;
  lift_angle_used: number;
  temperature_c: number | null;
  power_state: string | null;
  device_name: string | null;
  sample_rate_hz: number | null;
  clip_seconds: number | null;
  quality: number | null;
  beats_used: number | null;
  source_audio_path: string | null;
  notes: string | null;
}

export interface TestEdit {
  position: string | null;
  temperature_c: number | null;
  power_state: string | null;
  notes: string | null;
}

export interface RecordArgs {
  deviceName: string | null;
  bph: number;
  liftAngleDeg: number;
  seconds: number;
  saveRecording: boolean;
}

export const api = {
  health: () => invoke<{ app_version: string; core_version: string }>("health"),
  listInputDevices: () => invoke<DeviceInfo[]>("list_input_devices"),
  recordAndAnalyze: (args: RecordArgs) =>
    invoke<MeasurementDto>("record_and_analyze", { ...args }),
  analyzeFile: (path: string, bph: number, liftAngleDeg: number) =>
    invoke<MeasurementDto>("analyze_file", { path, bph, liftAngleDeg }),

  getSettings: () => invoke<Settings>("get_settings"),
  setDataDir: (path: string) => invoke<Settings>("set_data_dir", { path }),
  setDefaultClip: (seconds: number) =>
    invoke<Settings>("set_default_clip", { seconds }),

  listWatches: () => invoke<Watch[]>("list_watches"),
  createWatch: (input: WatchInput) => invoke<Watch>("create_watch", { input }),
  updateWatch: (id: string, input: WatchInput) =>
    invoke<Watch>("update_watch", { id, input }),
  deleteWatch: (id: string) => invoke<void>("delete_watch", { id }),

  listTests: (watchId: string) => invoke<Test[]>("list_tests", { watchId }),
  saveTest: (watchId: string, input: TestInput) =>
    invoke<Test>("save_test", { watchId, input }),
  updateTest: (id: string, edit: TestEdit) =>
    invoke<Test>("update_test", { id, edit }),
  deleteTest: (id: string) => invoke<void>("delete_test", { id }),
};

export function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
}
