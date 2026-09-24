import type { Limit, PercentageMode, ProviderId, ProviderState, RecoveryPhase, Settings } from "./types";
import { eligibleQuota, hiddenLowQuota, miniLimits, quotaPercent, remainingPercent, sampleAge, shortDuration, wholePercent } from "./usage";

export type MiniLayout = "columns" | "stacked";

const names: Record<ProviderId, string> = { claude: "Claude", codex: "Codex" };
const sources: Record<Limit["provenance"], string> = {
  official: "Official", local_estimate: "Local estimate", unknown: "Unknown source",
};

function MiniReading({ limit, heading, primary, mode, threshold, now }: {
  limit: Limit | undefined; heading: string; primary: boolean; mode: PercentageMode; threshold: number; now: number;
}) {
  const valid = limit !== undefined && eligibleQuota(limit, now);
  const remaining = valid ? remainingPercent(limit) : null;
  const percentage = remaining === null ? null : quotaPercent(remaining, mode);
  const low = remaining !== null && remaining < threshold;
  const expired = limit?.resets_at !== null && limit?.resets_at !== undefined && limit.resets_at <= now;
  const reset = limit?.resets_at ?? null;
  const resetText = reset === null ? "No reset time" : expired ? "Awaiting reset" : `Resets ${shortDuration(reset - now)}`;

  return (
    <div className={`mini-reading${low ? " low" : ""}`}>
      <div className="mini-heading">
        {primary ? <h2>{heading}</h2> : <h3>{heading}</h3>}
        {percentage !== null && <span className="mini-percentage">{wholePercent(percentage, mode)}<span>%</span></span>}
      </div>
      {percentage !== null && limit ? (
        <>
          <div className={`usage-bar${low ? " low" : ""}`} role="progressbar"
            aria-label={`${limit.label}, ${mode}`} aria-valuenow={percentage} aria-valuemin={0} aria-valuemax={100}>
            <div className="usage-fill" style={{ width: `${percentage}%` }} />
          </div>
          <div className="mini-meta">
            {primary && <span>{limit.label}</span>}
            <span title={reset === null ? undefined : new Date(reset * 1000).toLocaleString()}>{resetText}</span>
          </div>
        </>
      ) : <p className="mini-unavailable">{!limit ? "Chosen window unavailable" : expired ? "Awaiting reset update" : "Usage unavailable"}</p>}
    </div>
  );
}

export function MiniView({ providers, settings, now, expanded, pendingRecovery, onExpand, onDetails }: {
  providers: ProviderState[]; settings: Settings; now: number; expanded: boolean;
  pendingRecovery: Partial<Record<ProviderId, RecoveryPhase>>;
  onExpand: () => void; onDetails: () => void;
}) {
  return (
    <div className="mini-grid" id="mini-limits">
      {providers.map((provider) => {
        const enabled = provider.id === "claude" ? settings.claude_enabled : settings.codex_enabled;
        const recovery = pendingRecovery[provider.id] ?? provider.recovery;
        const snapshot = enabled && !recovery ? provider.snapshot : null;
        const selected = provider.id === "claude" ? settings.claude_windows : settings.codex_windows;
        const slots = snapshot ? miniLimits(provider.id, snapshot.limits, selected, settings.tracked_limit, now) : [];
        const shown = expanded ? slots : slots.slice(0, 1);
        const hiddenLow = snapshot ? hiddenLowQuota(snapshot.limits, shown, settings.threshold, now) : undefined;
        const hiddenRemaining = hiddenLow ? remainingPercent(hiddenLow) : null;
        const expired = shown.some((limit) => limit?.resets_at !== null && limit?.resets_at !== undefined && limit.resets_at <= now);
        const stale = snapshot !== null && (provider.stale || provider.error !== null || expired);
        const provenance = [...new Set(shown.flatMap((limit) => limit ? [limit.provenance] : []))];
        const status = recovery ? recovery === "signing_in" ? "Finish signing in" : "Reconnecting"
          : !enabled ? "Disconnected"
          : snapshot ? stale ? `Stale · ${sampleAge(snapshot.fetched_at, now)}` : provider.refreshing ? "Checking" : null
          : provider.error ? ["sign_in", "authentication", "recovery"].includes(provider.error.kind) ? "Sign in needed"
            : provider.error.kind === "rate_limited" ? "Waiting to retry" : "Usage unavailable"
          : "Reading usage";
        return (
          <section key={provider.id} className={`mini-provider ${provider.id}`} aria-label={`${names[provider.id]} usage`}
            aria-busy={provider.refreshing || recovery !== null}>
            {slots.length > 0 ? shown.map((limit, index) => (
              <MiniReading key={index === 0 ? "first" : "second"} limit={limit} primary={index === 0}
                heading={index === 0 ? names[provider.id] : limit?.label ?? "Second window"}
                mode={settings.percentage_mode} threshold={settings.threshold} now={now} />
            )) : <div className="mini-heading"><h2>{names[provider.id]}</h2></div>}
            {snapshot && slots.length === 0 && <p className="mini-unavailable">No quota windows</p>}
            {hiddenLow && hiddenRemaining !== null && (
              <button className="mini-warning" title={`Show ${hiddenLow.label}`} onClick={() => {
                if (!expanded && slots[1]?.id === hiddenLow.id) onExpand();
                else onDetails();
              }}>
                {hiddenLow.label}: {wholePercent(quotaPercent(hiddenRemaining, settings.percentage_mode), settings.percentage_mode)}% {settings.percentage_mode}
              </button>
            )}
            <div className="mini-source" title={snapshot ? `Updated ${sampleAge(snapshot.fetched_at, now)}` : undefined}>
              {provenance.map((source) => <span key={source} className={`provenance ${source}`}>{sources[source]}</span>)}
              {status && <button className={`mini-status${stale || provider.error ? " stale" : ""}`}
                title={provider.error?.message ?? "Open full view"} onClick={onDetails}>{status}</button>}
              {!snapshot && provider.next_retry_at !== null && provider.next_retry_at > now && (
                <span>Retry in {shortDuration(provider.next_retry_at - now)}</span>
              )}
              {!snapshot && <button className="mini-details" onClick={onDetails}>Open details</button>}
            </div>
          </section>
        );
      })}
    </div>
  );
}
