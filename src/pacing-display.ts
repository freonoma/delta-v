import type { AllowancePace } from "./pacing";
import type { PercentageMode } from "./types";

export type ReadyPace = Extract<AllowancePace, { state: "ready" }>;

export function paceStatus(pace: ReadyPace): { label: string; tone: "above" | "near" | "early" | "exhausted" } {
  if (pace.usedFraction === 1) return { label: "Used up", tone: "exhausted" };
  // Keep this margin and settling period aligned with the native pace alerts.
  if (pace.windowSeconds - pace.remainingSeconds < Math.min(6 * 3600, pace.windowSeconds * 0.1)) {
    return { label: "Just started", tone: "early" };
  }
  if (pace.deltaFraction > 0.1 + 1e-9) return { label: "Watch usage", tone: "above" };
  return { label: "On track", tone: "near" };
}

export function paceExplanation(pace: ReadyPace): string {
  if (pace.usedFraction === 1) return "This window’s allowance is used up.";
  return `${pacePercent(pace.remainingFraction * 100)} left for the next ${paceDuration(pace.remainingSeconds)}.`;
}

export function paceDuration(seconds: number): string {
  const minutes = Math.max(1, Math.ceil(seconds / 60));
  const hours = Math.floor(minutes / 60);
  const days = Math.floor(hours / 24);
  const unit = (value: number, name: string) => `${value} ${name}${value === 1 ? "" : "s"}`;
  if (days) return unit(days, "day") + (hours % 24 ? `, ${unit(hours % 24, "hour")}` : "");
  if (hours) return unit(hours, "hour") + (minutes % 60 ? `, ${unit(minutes % 60, "minute")}` : "");
  return unit(minutes, "minute");
}

export function paceMarkerPercent(pace: ReadyPace, mode: PercentageMode): number {
  return (mode === "remaining" ? pace.guideRemainingFraction : pace.elapsedFraction) * 100;
}

export function pacePercent(value: number): string {
  if (value > 0 && value < 0.1) return "<0.1%";
  if (value > 99.9 && value < 100) return ">99.9%";
  return `${Number(value.toFixed(1))}%`;
}

export function paceBudget(pace: ReadyPace): string {
  if (pace.usedFraction === 1) return "No allowance left";
  const quota = pace.windowSeconds === 604800 ? "weekly allowance" : "allowance for this window";
  if (pace.remainingSeconds >= 86400) return `About ${pacePercent(pace.remainingPerDay)} of your ${quota} per day.`;
  if (pace.remainingSeconds >= 3600) return `About ${pacePercent(pace.remainingPerHour)} of your ${quota} per hour.`;
  return `${pacePercent(pace.remainingFraction * 100)} of allowance left`;
}

export function paceUnavailable(reason: Extract<AllowancePace, { state: "unavailable" }>["reason"]): string {
  switch (reason) {
    case "freshness": return "Waiting for fresh usage";
    case "blocked": return "Pace paused";
    case "elapsed": return "Waiting for reset update";
    case "window": return "Window timing unavailable";
    case "reset": return "Reset time unavailable";
    case "usage": return "Usage unavailable";
    case "clock": return "Pace unavailable";
    case "unsupported": return "Pace unavailable";
  }
}
