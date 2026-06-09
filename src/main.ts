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
  sample_rate: number;
  device_name: string;
  clip_seconds: number;
  peak_level: number;
  degraded_input: boolean;
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
  el("m-rate").textContent = formatRate(m.rate_s_per_day);
  el("m-beaterror").textContent = formatBeatError(m.beat_error_ms);
  el("m-amplitude").textContent = formatAmplitude(m.amplitude_deg);

  el("result-meta").textContent =
    `${m.beats_detected} beats · ${(m.sample_rate / 1000).toFixed(1)} kHz · ` +
    `${m.clip_seconds.toFixed(0)} s · ${m.bph} bph · ${m.lift_angle_deg}° lift · ${m.device_name}`;

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

  button.disabled = true;
  status.textContent = `Recording for ${seconds.toFixed(0)} s — hold the microphone against the watch…`;
  try {
    const m = await invoke<MeasurementDto>("record_and_analyze", {
      deviceName,
      bph,
      liftAngleDeg,
      seconds,
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
