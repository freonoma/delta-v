import { createPreview } from "../../src/preview";

export type Scenario = "typical" | "half" | "low" | "exhausted" | "stale" | "before-reset" | "refilled";

export function sampleReadings(scenario: Scenario, deadline = Math.floor(Date.now() / 1000)) {
  const now = scenario === "before-reset" ? deadline - 60 : deadline;
  const observed = scenario === "stale" ? now - 480 : now;
  const fixture = createPreview(scenario === "stale" ? "?pace=weekly&stale=both" : "?pace=weekly");
  const providers = fixture.providers.map((provider) => {
    const snapshot = provider.snapshot;
    if (!snapshot) return provider;
    const limits = snapshot.limits.map((limit) => {
      if (limit.kind !== "quota" || limit.window_seconds === null) return limit;
      if (scenario === "exhausted") return { ...limit, used_fraction: 1 };
      if (scenario === "half") return { ...limit, used_fraction: 0.5 };
      if (scenario === "low") return { ...limit, used_fraction: provider.id === "claude" ? 0.88 : 0.93 };
      if (scenario === "before-reset") return { ...limit, used_fraction: 1, resets_at: deadline };
      if (scenario === "refilled") return { ...limit, used_fraction: 0, resets_at: deadline + limit.window_seconds };
      return limit;
    });
    return { ...provider, reading_scope: "study", reading_id: `${provider.id}:${observed}:${scenario}`,
      snapshot: { ...snapshot, limits, fetched_at: observed } };
  });
  return { now, providers };
}
