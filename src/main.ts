import { invoke } from "@tauri-apps/api/core";
import { formatVersions } from "./format";

interface Health {
  app_version: string;
  core_version: string;
}

async function showHealth(): Promise<void> {
  const el = document.querySelector<HTMLParagraphElement>("#health");
  if (!el) return;
  try {
    const health = await invoke<Health>("health");
    el.textContent = formatVersions(health.app_version, health.core_version);
  } catch (err) {
    el.textContent = `Backend unavailable: ${String(err)}`;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  void showHealth();
});
