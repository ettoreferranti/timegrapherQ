import { invoke } from "@tauri-apps/api/core";
import {
  formatAmplitude,
  formatBeatError,
  formatRate,
  formatVersions,
} from "./format";

interface DeviceInfo {
  name: string;
  is_default: boolean;
  default_sample_rate: number;
  channels: number;
}

interface MeasurementDto {
  rate_s_per_day: number;
  beat_error_ms: number;
  amplitude_deg: number | null;
  bph: number;
  lift_angle_deg: number;
  beats_detected: number;
  beats_used: number;
  beats_expected: number;
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

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`missing element #${id}`);
  return node as T;
}

async function loadDevices(): Promise<void> {
  const select = el<HTMLSelectElement>("device");
  const note = el<HTMLParagraphElement>("device-note");
  try {
    const devices = await invoke<DeviceInfo[]>("list_input_devices");
    select.innerHTML = "";
    if (devices.length === 0) {
      note.textContent = "No input devices found.";
      return;
    }
    for (const d of devices) {
      const opt = document.createElement("option");
      opt.value = d.name;
      const rate = d.default_sample_rate
        ? `${(d.default_sample_rate / 1000).toFixed(1)} kHz`
        : "unknown rate";
      opt.textContent = `${d.name}${d.is_default ? " (default)" : ""} — ${rate}`;
      if (d.is_default) opt.selected = true;
      select.appendChild(opt);
    }
    note.textContent =
      "Tip: a wired/contact mic gives the cleanest reading. Bluetooth mics often drop to a low-quality profile.";
  } catch (err) {
    note.textContent = `Could not list devices: ${String(err)}`;
  }
}

function showMetrics(m: MeasurementDto): void {
  el("m-rate").textContent = m.measured ? formatRate(m.rate_s_per_day) : "—";
  el("m-beaterror").textContent = m.measured
    ? formatBeatError(m.beat_error_ms)
    : "—";
  el("m-amplitude").textContent = m.measured
    ? formatAmplitude(m.amplitude_deg)
    : "—";

  el("result-meta").textContent = m.measured
    ? `confidence ${Math.round(m.quality * 100)}% · ${m.beats_used}/${m.beats_expected} ticks · ` +
      `${(m.sample_rate / 1000).toFixed(1)} kHz · ${m.clip_seconds.toFixed(0)} s · ` +
      `${m.bph} bph · ${m.lift_angle_deg}° lift · ${m.device_name}`
    : `no measurement · ${(m.sample_rate / 1000).toFixed(1)} kHz · ${m.clip_seconds.toFixed(0)} s · ${m.device_name}`;

  // Diagnostics line: helps tell a capture problem from a maths problem.
  const diag = el<HTMLParagraphElement>("diag");
  const parts = [
    `${m.raw_onsets} transients (~${m.beats_expected} expected)`,
    `peak ${m.peak_level.toFixed(3)}`,
    `rms ${m.rms_level.toFixed(4)}`,
  ];
  if (m.recording_path) parts.push(`saved: ${m.recording_path}`);
  diag.textContent = parts.join(" · ");

  el("result").classList.toggle(
    "low-confidence",
    !m.measured || m.quality < 0.6,
  );

  const warning = el<HTMLParagraphElement>("warning");
  if (m.warning) {
    warning.textContent = `⚠ ${m.warning}`;
    warning.classList.remove("hidden");
  } else {
    warning.classList.add("hidden");
  }
  el("result").classList.remove("hidden");
}

async function onRecord(): Promise<void> {
  const button = el<HTMLButtonElement>("record");
  const status = el<HTMLParagraphElement>("status");
  const deviceName = el<HTMLSelectElement>("device").value || null;
  const bph = Number(el<HTMLSelectElement>("bph").value);
  const liftAngleDeg = Number(el<HTMLInputElement>("lift").value);
  const seconds = Number(el<HTMLInputElement>("duration").value);
  const saveRecording = el<HTMLInputElement>("save").checked;

  button.disabled = true;
  status.textContent = `Recording for ${seconds.toFixed(0)} s — hold the microphone against the watch…`;
  try {
    const m = await invoke<MeasurementDto>("record_and_analyze", {
      deviceName,
      bph,
      liftAngleDeg,
      seconds,
      saveRecording,
    });
    status.textContent = "";
    showMetrics(m);
  } catch (err) {
    status.textContent = `✕ ${String(err)}`;
  } finally {
    button.disabled = false;
  }
}

async function showFooter(): Promise<void> {
  try {
    const h = await invoke<{ app_version: string; core_version: string }>(
      "health",
    );
    el("footer").textContent = formatVersions(h.app_version, h.core_version);
  } catch {
    el("footer").textContent = "Backend unavailable.";
  }
}

window.addEventListener("DOMContentLoaded", () => {
  el<HTMLButtonElement>("record").addEventListener("click", () => {
    void onRecord();
  });
  el<HTMLButtonElement>("refresh").addEventListener("click", () => {
    void loadDevices();
  });
  void loadDevices();
  void showFooter();
});
