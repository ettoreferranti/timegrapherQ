import { open } from "@tauri-apps/plugin-dialog";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { openPath } from "@tauri-apps/plugin-opener";
import {
  api,
  el,
  type LiveBeat,
  type LiveBeatsBatch,
  type LiveMetrics,
  type MicLevel,
  type MeasurementDto,
  type Test,
  type Watch,
  type WatchInput,
} from "./api";
import {
  formatAmplitude,
  formatBeatError,
  formatRate,
  formatVersions,
} from "./format";

let lastMeasurement: MeasurementDto | null = null;
let editingWatchId: string | null = null;
let currentWatch: Watch | null = null;

// ---------- navigation ----------

function switchView(name: string): void {
  for (const v of document.querySelectorAll<HTMLElement>(".view")) {
    v.classList.toggle("hidden", v.id !== `view-${name}`);
  }
  for (const t of document.querySelectorAll<HTMLButtonElement>(".tab")) {
    t.classList.toggle("active", t.dataset.view === name);
  }
  if (name === "collection") void refreshWatches();
  if (name === "settings") void loadSettings();
  if (name === "measure") void populateSaveWatches();
}

// ---------- measure ----------

async function loadDevices(): Promise<void> {
  const select = el<HTMLSelectElement>("device");
  const note = el<HTMLParagraphElement>("device-note");
  try {
    const devices = await api.listInputDevices();
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
      "Tip: a wired/contact mic gives the cleanest reading. Bluetooth/phone mics filter out ticks.";
  } catch (err) {
    note.textContent = `Could not list devices: ${String(err)}`;
  }
}

function showMetrics(m: MeasurementDto): void {
  el("m-rate").textContent = m.measured
    ? `${formatRate(m.rate_s_per_day)} ±${m.rate_ci95_s_per_day.toFixed(1)}`
    : "—";
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

  const parts = [
    `periodicity ${m.periodicity.toFixed(2)}`,
    m.detected_bph != null
      ? `detected ~${Math.round(m.detected_bph)} bph`
      : "no periodic tick",
    `${m.raw_onsets} transients (~${m.beats_expected} expected)`,
    `band ${(m.band_center_hz / 1000).toFixed(1)} kHz`,
    `peak ${m.peak_level.toFixed(3)}`,
    `rms ${m.rms_level.toFixed(4)}`,
  ];
  if (m.masked_seconds > 0)
    parts.push(`ignored ${m.masked_seconds.toFixed(1)} s of loud noise`);
  if (m.recording_path) parts.push(`saved: ${m.recording_path}`);
  el("diag").textContent = parts.join(" · ");

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

  // Offer saving only when we actually measured something.
  el("save-panel").classList.toggle("hidden", !m.measured);
  if (m.measured) void populateSaveWatches();
}

async function onRecord(): Promise<void> {
  const button = el<HTMLButtonElement>("record");
  const status = el<HTMLParagraphElement>("status");
  const seconds = Number(el<HTMLInputElement>("duration").value);

  // Recording, live mode and the mic monitor all share the microphone.
  await stopLive();
  await stopMonitor();

  button.disabled = true;
  status.textContent = `Recording for ${seconds.toFixed(0)} s — hold the microphone against the watch…`;
  try {
    const m = await api.recordAndAnalyze({
      deviceName: el<HTMLSelectElement>("device").value || null,
      bph: Number(el<HTMLSelectElement>("bph").value),
      liftAngleDeg: Number(el<HTMLInputElement>("lift").value),
      seconds,
      saveRecording: el<HTMLInputElement>("save").checked,
    });
    lastMeasurement = m;
    status.textContent = "";
    showMetrics(m);
  } catch (err) {
    status.textContent = `✕ ${String(err)}`;
  } finally {
    button.disabled = false;
  }
}

async function onAnalyzeFile(): Promise<void> {
  const button = el<HTMLButtonElement>("analyze-file");
  const status = el<HTMLParagraphElement>("status");
  const path = await open({
    multiple: false,
    directory: false,
    filters: [
      {
        name: "Audio",
        extensions: ["wav", "m4a", "aac", "mp3", "flac", "aiff", "aif", "ogg"],
      },
    ],
  });
  if (!path) return; // cancelled

  button.disabled = true;
  status.textContent = "Analyzing file…";
  try {
    const m = await api.analyzeFile(
      path,
      Number(el<HTMLSelectElement>("bph").value),
      Number(el<HTMLInputElement>("lift").value),
    );
    lastMeasurement = m;
    status.textContent = "";
    showMetrics(m);
  } catch (err) {
    status.textContent = `✕ ${String(err)}`;
  } finally {
    button.disabled = false;
  }
}

// ---------- mic check ----------

/** Bottom of the level meter (dBFS); faint watch ticks sit around −40 dB. */
const METER_FLOOR_DB = -60;

let monitorRunning = false;
let monitorUnlisten: UnlistenFn | null = null;
let monitorListen = false;
/** Peak-hold level (dBFS) that decays between meter frames. */
let peakHoldDb = METER_FLOOR_DB;

async function onToggleMonitor(): Promise<void> {
  if (monitorRunning) {
    await stopMonitor();
    return;
  }
  await stopLive(); // shared microphone
  await startMonitor();
}

async function startMonitor(): Promise<void> {
  const status = el<HTMLParagraphElement>("status");
  try {
    if (!monitorUnlisten) {
      monitorUnlisten = await listen<MicLevel>("mic-level", (e) =>
        drawMeter(e.payload),
      );
    }
    await api.startMicMonitor(
      el<HTMLSelectElement>("device").value || null,
      monitorListen,
      Number(el<HTMLInputElement>("mic-gain").value),
    );
    monitorRunning = true;
    el<HTMLButtonElement>("mic-check").textContent = "Stop";
    el("mic-listen-row").classList.remove("hidden");
    status.textContent = "";
  } catch (err) {
    await stopMonitor();
    status.textContent = `✕ ${String(err)}`;
  }
}

async function stopMonitor(): Promise<void> {
  if (monitorUnlisten) {
    monitorUnlisten();
    monitorUnlisten = null;
  }
  if (monitorRunning) {
    try {
      await api.stopMicMonitor();
    } catch {
      // already gone
    }
  }
  monitorRunning = false;
  el<HTMLButtonElement>("mic-check").textContent = "Mic check";
  el("mic-listen-row").classList.add("hidden");
  resetMeter();
}

/** Toggle audible passthrough; restart the running session to (de)activate it. */
async function onListenToggle(): Promise<void> {
  monitorListen = el<HTMLInputElement>("mic-listen").checked;
  if (monitorRunning) {
    // The backend stops the previous session when a new one starts.
    await api.startMicMonitor(
      el<HTMLSelectElement>("device").value || null,
      monitorListen,
      Number(el<HTMLInputElement>("mic-gain").value),
    );
  }
}

function onGainInput(): void {
  if (monitorRunning && monitorListen) {
    void api.setMonitorGain(Number(el<HTMLInputElement>("mic-gain").value));
  }
}

function drawMeter(level: MicLevel): void {
  const widthPct = dbToPct(level.db);
  // Decay the peak-hold ~45 dB/s (1.5 dB per ~33 ms frame).
  peakHoldDb = Math.max(level.db, peakHoldDb - 1.5);
  el("mic-meter-cover").style.width = `${100 - widthPct}%`;
  el("mic-meter-peak").style.left = `${dbToPct(peakHoldDb)}%`;
  el("mic-db").textContent = level.clipping
    ? "CLIP"
    : `${Math.round(level.db)} dB`;
  el("mic-meter").classList.toggle("clip", level.clipping);
}

/** Map a dBFS value onto 0–100% of the meter (floor..0 dB). */
function dbToPct(db: number): number {
  const pct = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB) * 100;
  return Math.max(0, Math.min(100, pct));
}

function resetMeter(): void {
  peakHoldDb = METER_FLOOR_DB;
  el("mic-meter-cover").style.width = "100%";
  el("mic-meter-peak").style.left = "0%";
  el("mic-db").textContent = "—";
  el("mic-meter").classList.remove("clip");
}

// ---------- live trace ----------

/** Seconds of trace kept on screen. */
const TRACE_SPAN_S = 30;

let liveRunning = false;
let liveUnlisteners: UnlistenFn[] = [];
let liveBeats: LiveBeat[] = [];
let liveBph = 28_800;

async function onToggleLive(): Promise<void> {
  if (liveRunning) {
    await stopLive();
    return;
  }
  await stopMonitor(); // shared microphone
  const status = el<HTMLParagraphElement>("status");
  liveBph = Number(el<HTMLSelectElement>("bph").value);
  liveBeats = [];
  try {
    liveUnlisteners.push(
      await listen<LiveBeatsBatch>("live-beats", (e) => {
        // The batch re-classifies the whole analysis window: replace our dots
        // in that range so colours heal as the window slides (beats outside
        // the window keep their last classification).
        const { window_start_s, beats } = e.payload;
        liveBeats = liveBeats
          .filter((b) => b.t_s < window_start_s)
          .concat(beats);
        const newest = liveBeats.length
          ? liveBeats[liveBeats.length - 1].t_s
          : 0;
        liveBeats = liveBeats.filter((b) => b.t_s >= newest - TRACE_SPAN_S - 5);
        drawTrace();
      }),
    );
    liveUnlisteners.push(
      await listen<LiveMetrics>("live-metrics", (e) =>
        updateLiveReadouts(e.payload),
      ),
    );
    await api.startLive(
      el<HTMLSelectElement>("device").value || null,
      liveBph,
      Number(el<HTMLInputElement>("lift").value),
    );
    liveRunning = true;
    el<HTMLButtonElement>("live-toggle").textContent = "Stop live";
    el("live").classList.remove("hidden");
    el("live-meta").textContent = "Listening…";
    status.textContent = "";
    drawTrace();
  } catch (err) {
    await stopLive();
    status.textContent = `✕ ${String(err)}`;
  }
}

async function stopLive(): Promise<void> {
  if (!liveRunning && liveUnlisteners.length === 0) return;
  try {
    await api.stopLive();
  } catch {
    // session may already be gone; nothing to do
  }
  for (const unlisten of liveUnlisteners) unlisten();
  liveUnlisteners = [];
  liveRunning = false;
  el<HTMLButtonElement>("live-toggle").textContent = "Live trace";
}

function updateLiveReadouts(m: LiveMetrics): void {
  el("live-rate").textContent = m.measured
    ? `${formatRate(m.rate_s_per_day)} ±${m.rate_ci95_s_per_day.toFixed(1)}`
    : "—";
  el("live-beaterror").textContent = m.measured
    ? formatBeatError(m.beat_error_ms)
    : "—";
  el("live-amplitude").textContent = m.measured
    ? formatAmplitude(m.amplitude_deg)
    : "—";

  const parts: string[] = [];
  if (m.warming_up) {
    parts.push(`warming up… ${m.elapsed_s.toFixed(0)} s`);
  } else if (!m.measured) {
    parts.push("no steady tick — press the microphone against the watch");
  } else {
    parts.push(`confidence ${Math.round(m.quality * 100)}%`);
    parts.push(`${m.beats_used}/${m.beats_expected} ticks`);
    parts.push(`band ${(m.band_center_hz / 1000).toFixed(1)} kHz`);
  }
  parts.push(`input ${levelBar(m.peak_level)}`);
  parts.push(`${m.elapsed_s.toFixed(0)} s`);
  el("live-meta").textContent = parts.join(" · ");

  el("live").classList.toggle("low-confidence", !m.measured || m.quality < 0.6);
}

/** A small textual VU bar for the input level. */
function levelBar(peak: number): string {
  // Map typical tick peaks (0.001..0.5, logarithmic) onto 0..8 blocks.
  const db = 20 * Math.log10(Math.max(peak, 1e-5));
  const filled = Math.max(0, Math.min(8, Math.round((db + 80) / 7.5)));
  return "▰".repeat(filled) + "▱".repeat(8 - filled);
}

function drawTrace(): void {
  const canvas = el<HTMLCanvasElement>("live-canvas");
  const dpr = window.devicePixelRatio || 1;
  const cssW = canvas.clientWidth || 600;
  const cssH = canvas.clientHeight || 240;
  if (canvas.width !== Math.round(cssW * dpr)) {
    canvas.width = Math.round(cssW * dpr);
    canvas.height = Math.round(cssH * dpr);
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, cssW, cssH);

  // y wraps every two beats (one full oscillation), so tick and tock form two
  // lines: slope = rate, vertical gap = beat error, off-line dots = noise.
  const wrapS = (3600 / liveBph) * 2;
  const newest = liveBeats.length
    ? liveBeats[liveBeats.length - 1].t_s
    : TRACE_SPAN_S;
  const t0 = Math.max(0, newest - TRACE_SPAN_S);

  ctx.strokeStyle = "rgba(255,255,255,0.08)";
  ctx.beginPath();
  ctx.moveTo(0, cssH / 2);
  ctx.lineTo(cssW, cssH / 2);
  ctx.stroke();

  for (const b of liveBeats) {
    if (b.t_s < t0) continue;
    const x = ((b.t_s - t0) / TRACE_SPAN_S) * cssW;
    const y = cssH - ((b.t_s % wrapS) / wrapS) * cssH;
    ctx.fillStyle = b.kept ? "#4ea1ff" : "rgba(255, 110, 110, 0.7)";
    ctx.fillRect(x - 1.5, y - 1.5, 3, 3);
  }
}

async function populateSaveWatches(): Promise<void> {
  const select = el<HTMLSelectElement>("save-watch");
  const note = el<HTMLParagraphElement>("save-watch-note");
  try {
    const watches = await api.listWatches();
    const previous = select.value;
    select.innerHTML = "";
    if (watches.length === 0) {
      note.textContent =
        "No watches yet — add one in the Collection tab to save measurements.";
    } else {
      note.textContent = "";
    }
    for (const w of watches) {
      const opt = document.createElement("option");
      opt.value = w.id;
      opt.textContent = w.brand ? `${w.name} (${w.brand})` : w.name;
      select.appendChild(opt);
    }
    if (previous) select.value = previous;
  } catch (err) {
    note.textContent = `Could not load watches: ${String(err)}`;
  }
}

async function onSaveTest(): Promise<void> {
  const status = el<HTMLParagraphElement>("save-status");
  const m = lastMeasurement;
  const watchId = el<HTMLSelectElement>("save-watch").value;
  if (!m || !m.measured) {
    status.textContent = "Record a measurement first.";
    return;
  }
  if (!watchId) {
    status.textContent = "Add and select a watch first (Collection tab).";
    return;
  }
  const keepClip = el<HTMLInputElement>("save-keep-clip").checked;
  const temp = el<HTMLInputElement>("save-temp").value;
  try {
    await api.saveTest(watchId, {
      position: el<HTMLSelectElement>("save-position").value || null,
      rate_s_per_day: m.rate_s_per_day,
      rate_ci95_s_per_day: m.rate_ci95_s_per_day,
      beat_error_ms: m.beat_error_ms,
      amplitude_deg: m.amplitude_deg,
      bph_used: m.bph,
      lift_angle_used: m.lift_angle_deg,
      temperature_c: temp === "" ? null : Number(temp),
      power_state: el<HTMLInputElement>("save-power").value || null,
      device_name: m.device_name,
      sample_rate_hz: m.sample_rate,
      clip_seconds: m.clip_seconds,
      quality: m.quality,
      beats_used: m.beats_used,
      source_audio_path: keepClip ? m.recording_path : null,
      notes: el<HTMLInputElement>("save-notes").value || null,
    });
    status.textContent = "✓ Saved to collection.";
    if (keepClip && !m.recording_path) {
      status.textContent =
        "✓ Saved (no clip — tick 'Save recording' before recording to keep audio).";
    }
  } catch (err) {
    status.textContent = `✕ ${String(err)}`;
  }
}

// ---------- collection ----------

async function refreshWatches(): Promise<void> {
  const list = el<HTMLUListElement>("watch-list");
  try {
    const watches = await api.listWatches();
    list.innerHTML = "";
    if (watches.length === 0) {
      list.innerHTML = `<li class="empty">No watches yet. Click "Add watch".</li>`;
      return;
    }
    for (const w of watches) {
      list.appendChild(renderWatchRow(w));
    }
  } catch (err) {
    list.innerHTML = `<li class="empty">Error: ${String(err)}</li>`;
  }
}

function renderWatchRow(w: Watch): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "list-row";
  const subtitle = [w.brand, w.model, w.calibre].filter(Boolean).join(" · ");
  const meta = document.createElement("div");
  meta.innerHTML = `<strong>${escapeHtml(w.name)}</strong><br><span class="note">${escapeHtml(
    subtitle,
  )}${subtitle ? " · " : ""}${w.bph} bph · ${w.lift_angle}°</span>`;
  const actions = document.createElement("div");
  actions.className = "row";
  actions.appendChild(button("Tests", "ghost", () => void selectWatch(w)));
  actions.appendChild(button("Edit", "ghost", () => openWatchForm(w)));
  actions.appendChild(button("Delete", "ghost", () => void onDeleteWatch(w)));
  li.append(meta, actions);
  return li;
}

function openWatchForm(w?: Watch): void {
  editingWatchId = w ? w.id : null;
  el("watch-form-title").textContent = w ? "Edit watch" : "Add watch";
  setVal("w-name", w?.name ?? "");
  setVal("w-brand", w?.brand ?? "");
  setVal("w-model", w?.model ?? "");
  setVal("w-calibre", w?.calibre ?? "");
  setVal("w-reference", w?.reference ?? "");
  setVal("w-serial", w?.serial ?? "");
  setVal("w-bph", String(w?.bph ?? 28800));
  setVal("w-lift", String(w?.lift_angle ?? 52));
  setVal("w-notes", w?.notes ?? "");
  el("watch-form-status").textContent = "";
  el("watch-form-panel").classList.remove("hidden");
}

async function onWatchSave(): Promise<void> {
  const status = el<HTMLParagraphElement>("watch-form-status");
  const input: WatchInput = {
    name: getVal("w-name").trim(),
    brand: getVal("w-brand") || null,
    model: getVal("w-model") || null,
    calibre: getVal("w-calibre") || null,
    reference: getVal("w-reference") || null,
    serial: getVal("w-serial") || null,
    bph: Number(getVal("w-bph")),
    lift_angle: Number(getVal("w-lift")),
    notes: getVal("w-notes") || null,
  };
  if (!input.name) {
    status.textContent = "Name is required.";
    return;
  }
  try {
    if (editingWatchId) await api.updateWatch(editingWatchId, input);
    else await api.createWatch(input);
    el("watch-form-panel").classList.add("hidden");
    await refreshWatches();
  } catch (err) {
    status.textContent = `✕ ${String(err)}`;
  }
}

async function onDeleteWatch(w: Watch): Promise<void> {
  if (!confirm(`Delete "${w.name}" and all its tests? This cannot be undone.`))
    return;
  await api.deleteWatch(w.id);
  if (currentWatch?.id === w.id) {
    el("tests-panel").classList.add("hidden");
    currentWatch = null;
  }
  await refreshWatches();
}

async function selectWatch(w: Watch): Promise<void> {
  currentWatch = w;
  el("tests-title").textContent = `Tests — ${w.name}`;
  el("tests-panel").classList.remove("hidden");
  const container = el<HTMLDivElement>("tests-table");
  try {
    const tests = await api.listTests(w.id);
    renderTests(container, tests);
  } catch (err) {
    container.textContent = `Error: ${String(err)}`;
  }
}

function renderTests(container: HTMLElement, tests: Test[]): void {
  if (tests.length === 0) {
    container.innerHTML = `<p class="note">No tests yet for this watch.</p>`;
    return;
  }
  const table = document.createElement("table");
  table.className = "tests";
  table.innerHTML = `<thead><tr>
    <th>When</th><th>Pos</th><th>Rate</th><th>Beat err</th><th>Ampl</th>
    <th>Temp</th><th>Conf</th><th>Notes</th><th></th></tr></thead>`;
  const tbody = document.createElement("tbody");
  for (const t of tests) {
    const tr = document.createElement("tr");
    const cells = [
      formatDate(t.measured_at),
      t.position ?? "",
      t.rate_s_per_day != null
        ? formatRate(t.rate_s_per_day) +
          (t.rate_ci95_s_per_day != null
            ? ` ±${t.rate_ci95_s_per_day.toFixed(1)}`
            : "")
        : "",
      t.beat_error_ms != null ? formatBeatError(t.beat_error_ms) : "",
      t.amplitude_deg != null ? formatAmplitude(t.amplitude_deg) : "",
      t.temperature_c != null ? `${t.temperature_c}°C` : "",
      t.quality != null ? `${Math.round(t.quality * 100)}%` : "",
      t.notes ?? "",
    ];
    for (const c of cells) {
      const td = document.createElement("td");
      td.textContent = c;
      tr.appendChild(td);
    }
    const actions = document.createElement("td");
    actions.appendChild(button("Notes", "ghost", () => void onEditTest(t)));
    actions.appendChild(button("✕", "ghost", () => void onDeleteTest(t)));
    tr.appendChild(actions);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  container.innerHTML = "";
  container.appendChild(table);
}

async function onEditTest(t: Test): Promise<void> {
  const notes = prompt("Notes for this test:", t.notes ?? "");
  if (notes === null) return; // cancelled
  await api.updateTest(t.id, {
    position: t.position,
    temperature_c: t.temperature_c,
    power_state: t.power_state,
    notes: notes || null,
  });
  if (currentWatch) await selectWatch(currentWatch);
}

async function onDeleteTest(t: Test): Promise<void> {
  if (!confirm("Delete this test?")) return;
  await api.deleteTest(t.id);
  if (currentWatch) await selectWatch(currentWatch);
}

// ---------- settings ----------

async function loadSettings(): Promise<void> {
  try {
    const s = await api.getSettings();
    el("data-dir").textContent = s.data_dir;
    setVal("default-clip", String(s.default_clip_seconds));
  } catch (err) {
    el("settings-status").textContent = `✕ ${String(err)}`;
  }
}

async function onChooseDir(): Promise<void> {
  const status = el<HTMLParagraphElement>("settings-status");
  try {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string") return;
    const s = await api.setDataDir(picked);
    el("data-dir").textContent = s.data_dir;
    status.textContent = "✓ Data location updated.";
    await refreshWatches();
  } catch (err) {
    status.textContent = `✕ ${String(err)}`;
  }
}

async function onRevealDir(): Promise<void> {
  try {
    await openPath(el("data-dir").textContent ?? "");
  } catch (err) {
    el("settings-status").textContent = `✕ ${String(err)}`;
  }
}

async function onSaveDefaults(): Promise<void> {
  const status = el<HTMLParagraphElement>("settings-status");
  try {
    await api.setDefaultClip(Number(getVal("default-clip")));
    setVal("duration", getVal("default-clip"));
    status.textContent = "✓ Saved.";
  } catch (err) {
    status.textContent = `✕ ${String(err)}`;
  }
}

// ---------- helpers ----------

function button(
  label: string,
  cls: string,
  onClick: () => void,
): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = cls;
  b.textContent = label;
  b.addEventListener("click", onClick);
  return b;
}

function getVal(id: string): string {
  return el<HTMLInputElement>(id).value;
}

function setVal(id: string, value: string): void {
  el<HTMLInputElement>(id).value = value;
}

function escapeHtml(s: string): string {
  return s.replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c] ?? c,
  );
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

async function showFooter(): Promise<void> {
  try {
    const h = await api.health();
    el("footer").textContent = formatVersions(h.app_version, h.core_version);
  } catch {
    el("footer").textContent = "Backend unavailable.";
  }
}

// ---------- bootstrap ----------

window.addEventListener("DOMContentLoaded", () => {
  for (const tab of document.querySelectorAll<HTMLButtonElement>(".tab")) {
    tab.addEventListener("click", () =>
      switchView(tab.dataset.view ?? "measure"),
    );
  }
  el<HTMLButtonElement>("analyze-file").addEventListener(
    "click",
    () => void onAnalyzeFile(),
  );
  el<HTMLButtonElement>("live-toggle").addEventListener(
    "click",
    () => void onToggleLive(),
  );
  el<HTMLButtonElement>("mic-check").addEventListener(
    "click",
    () => void onToggleMonitor(),
  );
  el<HTMLInputElement>("mic-listen").addEventListener(
    "change",
    () => void onListenToggle(),
  );
  el<HTMLInputElement>("mic-gain").addEventListener("input", () =>
    onGainInput(),
  );
  el<HTMLButtonElement>("record").addEventListener(
    "click",
    () => void onRecord(),
  );
  el<HTMLButtonElement>("refresh").addEventListener(
    "click",
    () => void loadDevices(),
  );
  el<HTMLButtonElement>("save-test").addEventListener(
    "click",
    () => void onSaveTest(),
  );

  el<HTMLButtonElement>("add-watch").addEventListener("click", () =>
    openWatchForm(),
  );
  el<HTMLButtonElement>("watch-save").addEventListener(
    "click",
    () => void onWatchSave(),
  );
  el<HTMLButtonElement>("watch-cancel").addEventListener("click", () =>
    el("watch-form-panel").classList.add("hidden"),
  );

  el<HTMLButtonElement>("choose-dir").addEventListener(
    "click",
    () => void onChooseDir(),
  );
  el<HTMLButtonElement>("reveal-dir").addEventListener(
    "click",
    () => void onRevealDir(),
  );
  el<HTMLButtonElement>("save-defaults").addEventListener(
    "click",
    () => void onSaveDefaults(),
  );

  void loadDevices();
  void showFooter();
  void applyDefaultClip();
});

async function applyDefaultClip(): Promise<void> {
  try {
    const s = await api.getSettings();
    setVal("duration", String(s.default_clip_seconds));
  } catch {
    /* leave the HTML default */
  }
}
