// Small pure formatting helpers. Kept dependency-free and unit-tested.

/** Health-line text for the backend/core versions. */
export function formatVersions(
  appVersion: string,
  coreVersion: string,
): string {
  return `TimegrapherQ v${appVersion} · core v${coreVersion}`;
}

/**
 * Format a rate deviation in seconds/day the way a timegrapher displays it:
 * an explicit sign and a fixed number of decimals (e.g. `+3.2 s/d`, `-0.5 s/d`).
 */
export function formatRate(secondsPerDay: number, decimals = 1): string {
  const sign = secondsPerDay > 0 ? "+" : secondsPerDay < 0 ? "−" : "±";
  return `${sign}${Math.abs(secondsPerDay).toFixed(decimals)} s/d`;
}
