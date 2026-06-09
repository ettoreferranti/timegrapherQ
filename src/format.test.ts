import { describe, expect, it } from "vitest";
import { formatRate, formatVersions } from "./format";

describe("formatVersions", () => {
  it("renders app and core versions", () => {
    expect(formatVersions("0.1.0", "0.1.0")).toBe(
      "TimegrapherQ v0.1.0 · core v0.1.0",
    );
  });
});

describe("formatRate", () => {
  it("prefixes a + for a fast watch", () => {
    expect(formatRate(3.2)).toBe("+3.2 s/d");
  });

  it("prefixes a minus for a slow watch", () => {
    expect(formatRate(-0.5)).toBe("−0.5 s/d");
  });

  it("uses ± for an exactly-zero rate", () => {
    expect(formatRate(0)).toBe("±0.0 s/d");
  });
});
