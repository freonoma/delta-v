import type { Limit, PercentageMode, ProviderId } from "./types";

export function usedPercent(limit: Limit): number | null {
  return limit.used_fraction !== null && Number.isFinite(limit.used_fraction)
    ? limit.used_fraction * 100 : null;
}

export function remainingPercent(limit: Limit): number | null {
  const used = usedPercent(limit);
  if (used === null) return null;
  const remaining = Math.min(100, Math.max(0, 100 - used));
  return Math.round(remaining * 1e9) / 1e9;
}

export function quotaPercent(remaining: number, mode: PercentageMode): number {
  return mode === "remaining" ? remaining : Math.round((100 - remaining) * 1e9) / 1e9;
}

export function wholePercent(percentage: number, mode: PercentageMode): number {
  return mode === "remaining" ? Math.ceil(percentage) : Math.floor(percentage);
}

export function eligibleQuota(limit: Limit, now: number): boolean {
  const used = usedPercent(limit);
  return limit.enabled && limit.kind === "quota" && limit.provenance === "official"
    && used !== null && used >= 0 && used <= 100
    && (limit.resets_at === null || limit.resets_at > now);
}

export function shortDuration(seconds: number): string {
  const minutes = Math.max(1, Math.ceil(seconds / 60));
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

export function sampleAge(timestamp: number, now: number): string {
  const elapsed = Math.max(0, now - timestamp);
  if (elapsed < 60) return "just now";
  return `${shortDuration(Math.floor(elapsed / 60) * 60)} ago`;
}

export function compactLimits(provider: ProviderId, limits: Limit[], featured: Limit | undefined, selected: string[]): Limit[] {
  const quotas = limits.filter((limit) => limit.kind === "quota" && limit.enabled);
  if (selected.length > 0) {
    return selected.flatMap((id) => {
      const limit = quotas.find((quota) => quota.id === id);
      return limit ? [limit] : [];
    });
  }
  const main = quotas.filter((limit) => provider === "claude"
    ? limit.id === "session" || limit.id === "weekly"
    : limit.id.startsWith("rate_limit:"));
  const visible = (main.length > 0 ? main : quotas).slice(0, 2);
  if (featured && !visible.some((limit) => limit.id === featured.id)) {
    if (visible.length === 2) visible[1] = featured;
    else visible.push(featured);
  }
  return visible;
}

export function featuredQuota(provider: ProviderId, limits: Limit[], tracked: string, now: number): Limit | undefined {
  const quotas = limits.filter((limit) => eligibleQuota(limit, now));
  const chosen = quotas.find((limit) => tracked === `${provider}:${limit.id}`);
  const tightest = quotas.reduce<Limit | undefined>((current, limit) =>
    !current || (usedPercent(limit) ?? 0) > (usedPercent(current) ?? 0) ? limit : current,
  undefined);
  return tracked.startsWith(`${provider}:`) ? chosen : tightest;
}

export function miniLimits(provider: ProviderId, limits: Limit[], selected: string[], tracked: string, now: number): Array<Limit | undefined> {
  if (selected.length > 0) {
    return selected.slice(0, 2).map((id) => limits.find((limit) =>
      limit.id === id && limit.kind === "quota" && limit.enabled,
    ));
  }
  return compactLimits(provider, limits, featuredQuota(provider, limits, tracked, now), []).slice(0, 2);
}

export function hiddenLowQuota(limits: Limit[], shown: Array<Limit | undefined>, threshold: number, now: number): Limit | undefined {
  const hidden = limits.filter((limit) => {
    if (!eligibleQuota(limit, now) || shown.some((visible) => visible?.id === limit.id)) return false;
    const remaining = remainingPercent(limit);
    return remaining !== null && remaining < threshold;
  });
  return hidden.reduce<Limit | undefined>((current, limit) =>
    !current || (usedPercent(limit) ?? 0) > (usedPercent(current) ?? 0) ? limit : current,
  undefined);
}
