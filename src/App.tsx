import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppState, Amount, Limit, PercentageMode, ProviderId, ProviderSelection, ProviderState, Settings, Theme } from "./types";
import { createPreview } from "./preview";

const native = isTauri();
const preview = import.meta.env.DEV && !native;
const providerNames: Record<ProviderId, string> = { claude: "Claude", codex: "Codex" };
const provenanceNames: Record<Limit["provenance"], string> = {
  official: "Official", local_estimate: "Local estimate", unknown: "Unknown source",
};

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "Something went wrong. Try refreshing.";
}

function usedPercent(limit: Limit): number | null {
  return limit.used_fraction !== null && Number.isFinite(limit.used_fraction)
    ? limit.used_fraction * 100 : null;
}

function remainingPercent(limit: Limit): number | null {
  const used = usedPercent(limit);
  if (used === null) return null;
  const remaining = Math.min(100, Math.max(0, 100 - used));
  return Math.round(remaining * 1e9) / 1e9;
}

function quotaPercent(remaining: number, mode: PercentageMode): number {
  return mode === "remaining" ? remaining : Math.round((100 - remaining) * 1e9) / 1e9;
}

function wholePercent(percentage: number, mode: PercentageMode): number {
  return mode === "remaining" ? Math.ceil(percentage) : Math.floor(percentage);
}

function eligibleQuota(limit: Limit, now: number): boolean {
  const used = usedPercent(limit);
  return limit.enabled && limit.kind === "quota" && limit.provenance === "official"
    && used !== null && used >= 0 && used <= 100
    && (limit.resets_at === null || limit.resets_at > now);
}

function visibleTrackedLimit(tracked: string, providers: ProviderSelection): string {
  return providers === "both" || tracked === "auto" || tracked.startsWith(`${providers}:`)
    ? tracked : "auto";
}

function shortDuration(seconds: number): string {
  const minutes = Math.max(1, Math.ceil(seconds / 60));
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

function sampleAge(timestamp: number, now: number): string {
  const elapsed = Math.max(0, now - timestamp);
  if (elapsed < 60) return "just now";
  return `${shortDuration(Math.floor(elapsed / 60) * 60)} ago`;
}

function amountText(value: string, currency: string | null): string {
  return currency ? `${value} ${currency}` : `${value} credits`;
}

function amountDescription(amount: Amount): string | null {
  if (amount.balance !== null) return `${amountText(amount.balance, amount.currency)} available`;
  if (amount.used !== null && amount.limit !== null) {
    return `${amountText(amount.used, amount.currency)} used of ${amountText(amount.limit, amount.currency)}`;
  }
  if (amount.used !== null) return `${amountText(amount.used, amount.currency)} used`;
  return null;
}

function Icon({ name, spinning = false }: { name: "refresh" | "settings" | "quit" | "clock" | "close" | "chevron"; spinning?: boolean }) {
  return (
    <svg className={spinning ? "icon spinning" : "icon"} viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {name === "refresh" && <><path d="M16.4 7A6.5 6.5 0 0 0 5 4.8L2.8 7M3.6 13A6.5 6.5 0 0 0 15 15.2l2.2-2.2" /><path d="M2.8 3.2V7h3.8m10.6 9.8V13h-3.8" /></>}
      {name === "settings" && <><path d="M4 3v14m6-14v14m6-14v14" /><path d="M2 7h4m2 6h4m2-8h4" strokeWidth="3" /></>}
      {name === "quit" && <><path d="M10 2.5v7M6 4.5a6.5 6.5 0 1 0 8 0" /></>}
      {name === "clock" && <><circle cx="10" cy="10" r="6.5" /><path d="M10 6v4l2.6 1.5" /></>}
      {name === "close" && <path d="m5 5 10 10M15 5 5 15" />}
      {name === "chevron" && <path d="m6 8 4 4 4-4" />}
    </svg>
  );
}

function ResetTime({ timestamp, now }: { timestamp: number | null; now: number }) {
  if (timestamp === null) return <span className="reset-time">No reset time</span>;
  return (
    <span className="reset-time" title={new Date(timestamp * 1000).toLocaleString()}>
      <Icon name="clock" />
      {timestamp <= now ? "Awaiting reset update" : `Resets in ${shortDuration(timestamp - now)}`}
    </span>
  );
}

function LimitRow({ limit, now, threshold, tracked, percentageMode }: { limit: Limit; now: number; threshold: number; tracked: boolean; percentageMode: PercentageMode }) {
  const used = usedPercent(limit);
  const remaining = remainingPercent(limit);
  const low = limit.enabled && remaining !== null && remaining < threshold;
  const mode = limit.kind === "quota" ? percentageMode : "used";
  const percentage = limit.kind === "quota" && remaining !== null ? quotaPercent(remaining, mode) : used;
  const hasBar = limit.enabled && percentage !== null && limit.kind !== "credits";
  const amount = limit.amount ? amountDescription(limit.amount) : null;
  const percentageLabel = percentage === null ? null : percentage > 0 && percentage < 1 ? "<1"
    : percentage > 99 && percentage < 100 ? ">99"
    : limit.kind === "quota" ? wholePercent(percentage, mode) : Math.round(percentage);
  return (
    <li className={`limit-row${!limit.enabled ? " disabled-limit" : ""}`}>
      <div className="limit-heading">
        <span className="limit-label">{limit.label}{tracked && <span className="tracked-indicator" title="Tracked in the menu bar" aria-label="Tracked in the menu bar" />}</span>
        {hasBar && <span className={`used-value${low ? " low" : ""}`}>{percentageLabel}% <span>{mode}</span></span>}
      </div>
      {hasBar && (
        <div className={`usage-bar${low ? " low" : ""}`} role="progressbar" aria-label={`${limit.label}, ${mode}`} aria-valuenow={Math.min(100, Math.max(0, percentage))} aria-valuemin={0} aria-valuemax={100}>
          <div className="usage-fill" style={{ width: `${Math.min(100, Math.max(0, percentage))}%` }} />
        </div>
      )}
      {amount && <p className="amount-description">{amount}</p>}
      {!hasBar && !amount && <p className="amount-description">{limit.enabled ? "Usage unavailable" : "Not enabled"}</p>}
      {limit.detail && limit.kind !== "quota" && <p className="limit-detail">{limit.detail}</p>}
      <div className="limit-meta">
        <ResetTime timestamp={limit.resets_at} now={now} />
        <span className={`provenance ${limit.provenance}`}>{provenanceNames[limit.provenance]}</span>
      </div>
    </li>
  );
}

function compactLimits(provider: ProviderId, limits: Limit[], featured: Limit | undefined, selected: string[]): Limit[] {
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

function ProviderDetails({ limits, warnings }: { limits: Limit[]; warnings: string[] }) {
  const unknown = limits.filter((limit) => limit.kind === "unknown");
  const notes = limits.filter((limit) => limit.kind === "quota" && limit.detail !== null);
  if (unknown.length === 0 && notes.length === 0 && warnings.length === 0) return null;
  return (
    <details className="provider-details">
      <summary>Provider details</summary>
      {unknown.length > 0 && (
        <div className="unknown-fields">
          <p>These fields are not recognized as usage limits and do not affect the menu bar.</p>
          <ul>{unknown.map((limit) => <li key={limit.id}><code>{limit.label}</code></li>)}</ul>
        </div>
      )}
      {notes.map((limit) => <p key={limit.id}><strong>{limit.label}:</strong> {limit.detail}</p>)}
      {warnings.map((warning) => <p key={warning}>{warning}</p>)}
    </details>
  );
}

function ProviderColumn({ provider, settings, now, expanded }: { provider: ProviderState; settings: Settings; now: number; expanded: boolean }) {
  const snapshot = provider.snapshot;
  const expired = snapshot?.limits.some((limit) =>
    limit.enabled && limit.kind === "quota" && limit.resets_at !== null && limit.resets_at <= now,
  ) ?? false;
  const stale = provider.stale || expired;
  const quotas = snapshot?.limits.filter((limit) => eligibleQuota(limit, now)) ?? [];
  const chosen = quotas.find((limit) => settings.tracked_limit === `${provider.id}:${limit.id}`);
  const tracksThisProvider = settings.tracked_limit.startsWith(`${provider.id}:`);
  const tightest = quotas.reduce<Limit | undefined>((current, limit) =>
    !current || (usedPercent(limit) ?? 0) > (usedPercent(current) ?? 0) ? limit : current,
  undefined);
  const featured = tracksThisProvider ? chosen : tightest;
  const recognized = snapshot?.limits.filter((limit) => limit.kind !== "unknown") ?? [];
  const selected = provider.id === "claude" ? settings.claude_windows : settings.codex_windows;
  const shown = expanded ? recognized : compactLimits(provider.id, recognized, featured, selected);
  const missingSelected = !expanded && snapshot !== null && selected.some((id) => !shown.some((limit) => limit.id === id));
  const remaining = featured ? remainingPercent(featured) : null;
  const percentage = remaining === null ? null : quotaPercent(remaining, settings.percentage_mode);
  const low = remaining !== null && remaining < settings.threshold;
  const status = provider.refreshing ? "Refreshing" : !snapshot ? "Not connected" : stale ? "Stale" : "Updated";
  const emptyHeadline = snapshot
    ? expired ? "Waiting for fresh usage" : tracksThisProvider ? "Tracked window unavailable" : "No quota reading"
    : provider.refreshing ? "Reading usage" : "No usage reading";
  const emptyDescription = snapshot
    ? expired ? "A quota window has reached its reset time" : tracksThisProvider ? "Choose another window in Settings" : "The provider has no usable quota percentage"
    : provider.refreshing ? "Checking your account limits" : "Connect through your CLI";
  return (
    <section className={`provider-column ${provider.id}`} aria-label={`${providerNames[provider.id]} usage`} aria-busy={provider.refreshing}>
      <div className="provider-heading">
        <div className="provider-title">
          <h2>{providerNames[provider.id]}</h2>
          {expanded && snapshot?.plan && <span className="plan-label">{snapshot.plan}</span>}
        </div>
        <span className={`connection-status${snapshot && stale ? " stale" : ""}${provider.refreshing ? " loading" : ""}${!snapshot ? " disconnected" : ""}`} title={snapshot ? `Updated ${sampleAge(snapshot.fetched_at, now)}` : status}>
          <span className="status-dot" />{status}
        </span>
      </div>
      <div className={`remaining-summary${low ? " low" : ""}`}>
        {percentage !== null ? (
          <>
            <div className="remaining-number">{wholePercent(percentage, settings.percentage_mode)}<span>%</span></div>
            <div className="remaining-copy">
              <span>{settings.percentage_mode}</span>
              <span className="featured-label">{featured?.label}</span>
            </div>
          </>
        ) : (
          <div className="empty-summary">
            {emptyHeadline}
            <span>{emptyDescription}</span>
          </div>
        )}
      </div>
      {provider.error && (
        <div className="notice error-notice" role="status">
          <p>{provider.error}</p>
          {provider.next_retry_at !== null && provider.next_retry_at > now && (
            <p className="retry-time">Retry in {shortDuration(provider.next_retry_at - now)}</p>
          )}
        </div>
      )}
      {stale && snapshot && <p className="stale-note">Showing the last reading from {sampleAge(snapshot.fetched_at, now)}.</p>}
      {missingSelected && <p className="selection-note">A chosen window is unavailable. Choose another in Settings or use Show more.</p>}
      {shown.length > 0 ? (
        <ul className="limit-list">
          {shown.map((limit) => (
            <LimitRow
              key={limit.id}
              limit={limit}
              now={now}
              threshold={settings.threshold}
              percentageMode={settings.percentage_mode}
              tracked={settings.tracked_limit === `${provider.id}:${limit.id}`}
            />
          ))}
        </ul>
      ) : !provider.error && !missingSelected && (
        <div className="empty-state">
          <span className="empty-rule" />
          <p>{provider.refreshing ? "Your limits will appear here." : snapshot ? "No quota windows are available." : `Sign in to ${providerNames[provider.id]} ${provider.id === "claude" ? "Code" : "CLI"}, then choose Check now.`}</p>
        </div>
      )}
      {expanded && snapshot && (
        <>
          <ProviderDetails limits={snapshot.limits} warnings={snapshot.warnings} />
          <p className="provider-timestamp">Updated {sampleAge(snapshot.fetched_at, now)}</p>
        </>
      )}
    </section>
  );
}

function WindowPicker({ provider, selected, onChange }: { provider: ProviderState; selected: string[]; onChange: (windows: string[]) => void }) {
  const name = providerNames[provider.id];
  const available = provider.snapshot?.limits.filter((limit) => limit.kind === "quota" && limit.enabled) ?? [];
  const first = selected[0] ?? "";
  const second = selected[1] ?? "";
  const missing = selected.filter((id) => !available.some((limit) => limit.id === id));
  return (
    <fieldset className="window-picker">
      <legend>{name}</legend>
      <div className="window-picker-fields">
        <label>
          <span>First window</span>
          <select aria-label={`${name} first window`} value={first} onChange={(event) => {
            const value = event.target.value;
            onChange(value ? [value, ...selected.slice(1).filter((id) => id !== value)] : []);
          }}>
            <option value="">Automatic</option>
            {available.map((limit) => <option key={limit.id} value={limit.id}>{limit.label}</option>)}
            {missing.map((id) => <option key={id} value={id}>{id} (unavailable)</option>)}
          </select>
        </label>
        <label>
          <span>Second window</span>
          <select aria-label={`${name} second window`} value={second} disabled={!first} onChange={(event) => {
            const value = event.target.value;
            onChange(value ? [first, value] : [first]);
          }}>
            <option value="">{first ? "None" : "Automatic"}</option>
            {available.filter((limit) => limit.id !== first).map((limit) => <option key={limit.id} value={limit.id}>{limit.label}</option>)}
            {missing.filter((id) => id !== first).map((id) => <option key={id} value={id}>{id} (unavailable)</option>)}
          </select>
        </label>
      </div>
      {!provider.snapshot && <p>Connect {name} to choose its windows.</p>}
    </fieldset>
  );
}

function SettingsPanel({ state, saving, now, onSave, onClose, onThemePreview }: { state: AppState; saving: boolean; now: number; onSave: (settings: Settings) => Promise<void>; onClose: () => void; onThemePreview: (theme: Theme | null) => void }) {
  const panel = useRef<HTMLElement>(null);
  const [draft, setDraft] = useState(state.settings);
  const [threshold, setThreshold] = useState(String(state.settings.threshold));
  const [interval, setIntervalValue] = useState(String(state.settings.refresh_seconds));
  const [validation, setValidation] = useState<string | null>(null);
  const options = state.providers
    .filter((provider) => state.settings.providers === "both" || state.settings.providers === provider.id)
    .flatMap((provider) => (provider.snapshot?.limits ?? [])
      .filter((limit) => eligibleQuota(limit, now))
      .map((limit) => ({ value: `${provider.id}:${limit.id}`, label: `${providerNames[provider.id]} · ${limit.label}` })));
  const tracked = visibleTrackedLimit(draft.tracked_limit, state.settings.providers);
  const missingTracked = tracked !== "auto" && !options.some((option) => option.value === tracked);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => panel.current?.scrollIntoView({ block: "start" }));
    return () => window.cancelAnimationFrame(frame);
  }, []);

  useEffect(() => () => onThemePreview(null), [onThemePreview]);

  async function save() {
    const parsedThreshold = Number(threshold);
    const parsedInterval = Number(interval);
    if (!threshold.trim() || !Number.isInteger(parsedThreshold) || parsedThreshold < 0 || parsedThreshold > 100) {
      setValidation("Enter a whole-number remaining threshold from 0 to 100.");
      return;
    }
    if (!interval.trim() || !Number.isInteger(parsedInterval) || parsedInterval < 30 || parsedInterval > 900) {
      setValidation("Enter a refresh interval from 30 to 900 seconds.");
      return;
    }
    setValidation(null);
    try {
      await onSave({ ...draft, providers: state.settings.providers, tracked_limit: tracked, threshold: parsedThreshold, refresh_seconds: parsedInterval });
      onClose();
    } catch (error: unknown) {
      setValidation(errorMessage(error));
    }
  }

  return (
    <section ref={panel} className="settings-panel" aria-label="Settings">
      <div className="settings-heading">
        <h2>Settings</h2>
        <button className="icon-button" onClick={onClose} aria-label="Close settings"><Icon name="close" /></button>
      </div>
      <label className="setting-row">
        <span>Menu bar tracks<small>Choose a usage window</small></span>
        <select value={tracked} onChange={(event) => setDraft({ ...draft, tracked_limit: event.target.value })}>
          <option value="auto">Most-used quota</option>
          {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
          {missingTracked && <option value={tracked}>{tracked} (unavailable)</option>}
        </select>
      </label>
      <div className="compact-settings">
        <h3>Compact view</h3>
        <p>Choose up to two windows per provider. Show more reveals the rest.</p>
        <div className={`window-picker-grid${state.settings.providers === "both" ? " two-providers" : ""}`}>
          {state.providers.filter((provider) => state.settings.providers === "both" || state.settings.providers === provider.id).map((provider) => {
            const key = provider.id === "claude" ? "claude_windows" : "codex_windows";
            return <WindowPicker key={provider.id} provider={provider} selected={draft[key]} onChange={(windows) => setDraft({ ...draft, [key]: windows })} />;
          })}
        </div>
      </div>
      <label className="setting-row">
        <span>Show percentages as<small>Menu bar and quota bars</small></span>
        <select value={draft.percentage_mode} onChange={(event) => {
          const percentage_mode = event.target.value;
          if (percentage_mode === "remaining" || percentage_mode === "used") setDraft({ ...draft, percentage_mode });
        }}>
          <option value="remaining">Remaining</option>
          <option value="used">Used</option>
        </select>
      </label>
      <label className="setting-row">
        <span>Low budget threshold<small>Highlight when less than this percentage remains</small></span>
        <span className="number-field">
          <input type="number" min="0" max="100" step="1" inputMode="numeric" value={threshold} onChange={(event) => setThreshold(event.target.value)} />
          <span>%</span>
        </span>
      </label>
      <label className="setting-row">
        <span>Refresh interval<small>Backoff applies when rate limited</small></span>
        <span className="number-field">
          <input type="number" min="30" max="900" step="1" inputMode="numeric" value={interval} onChange={(event) => setIntervalValue(event.target.value)} />
          <span>sec</span>
        </span>
      </label>
      <label className="setting-row">
        <span>Appearance</span>
        <select value={draft.theme} onChange={(event) => {
          const theme = event.target.value;
          if (theme === "system" || theme === "light" || theme === "dark") {
            setDraft({ ...draft, theme });
            onThemePreview(theme);
          }
        }}>
          <option value="system">System</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </label>
      {validation && <p className="settings-validation" role="alert">{validation}</p>}
      <div className="settings-actions">
        <button className="text-button" onClick={onClose} disabled={saving}>Cancel</button>
        <button className="primary-button" onClick={() => void save()} disabled={saving}>{saving ? "Saving" : "Save settings"}</button>
      </div>
    </section>
  );
}

export default function App() {
  const [state, setState] = useState<AppState | null>(() => preview ? createPreview() : null);
  const [error, setError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [themePreview, setThemePreview] = useState<Theme | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [now, setNow] = useState(Math.floor(Date.now() / 1000));
  const shell = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const lastSize = useRef("");
  const selection = state?.settings.providers ?? "both";
  const theme: Theme = (settingsOpen ? themePreview : null) ?? state?.settings.theme ?? "system";

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    let receivedEvent = false;
    let unlisten: (() => void) | undefined;
    let unlistenOpen: (() => void) | undefined;
    void listen("popover-reset", () => {
      if (!disposed) {
        setExpanded(false);
        setSettingsOpen(false);
      }
    }).then((stop) => { if (disposed) stop(); else unlistenOpen = stop; }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    void listen<AppState>("usage-updated", (event) => {
      receivedEvent = true;
      if (!disposed) setState(event.payload);
    }).then((stop) => { if (disposed) stop(); else unlisten = stop; }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    void invoke<AppState>("get_state").then((initial) => { if (!disposed && !receivedEvent) setState(initial); }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    return () => { disposed = true; unlisten?.(); unlistenOpen?.(); };
  }, []);

  useEffect(() => {
    const element = shell.current;
    const scrollContent = content.current;
    if (!native || !element || !scrollContent) return;
    let frame = 0;
    const resize = () => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        const width = selection === "both" ? 560 : 340;
        const chromeHeight = Array.from(element.children)
          .filter((child) => !child.classList.contains("scroll-area"))
          .reduce((height, child) => height + child.getBoundingClientRect().height, 2);
        const height = Math.min(620, Math.max(180, Math.ceil(chromeHeight + scrollContent.getBoundingClientRect().height)));
        const size = `${width}:${height}`;
        if (lastSize.current === size) return;
        lastSize.current = size;
        void invoke("resize_popover", { width, height }).catch((caught: unknown) => setError(errorMessage(caught)));
      });
    };
    const observer = new ResizeObserver(resize);
    observer.observe(scrollContent);
    for (const child of element.children) {
      if (!child.classList.contains("scroll-area")) observer.observe(child);
    }
    resize();
    return () => { observer.disconnect(); window.cancelAnimationFrame(frame); };
  }, [selection]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (native) void invoke("hide_popover").catch((caught: unknown) => setError(errorMessage(caught)));
      else {
        setSettingsOpen(false);
        setExpanded(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const saveSettings = useCallback(async (settings: Settings) => {
    setSaving(true);
    try {
      if (preview) setState((current) => current ? { ...current, settings } : current);
      else setState(await invoke<AppState>("save_settings", { settings }));
      setError(null);
    } finally {
      setSaving(false);
    }
  }, []);

  async function selectProviders(providers: ProviderSelection) {
    if (!state || providers === state.settings.providers || saving) return;
    try {
      await saveSettings({
        ...state.settings,
        providers,
        tracked_limit: visibleTrackedLimit(state.settings.tracked_limit, providers),
      });
    }
    catch (caught: unknown) { setError(errorMessage(caught)); }
  }

  async function refresh() {
    setError(null);
    if (preview) {
      setState((current) => current ? { ...current, providers: current.providers.map((provider) => ({ ...provider, snapshot: provider.snapshot ? { ...provider.snapshot, fetched_at: Math.floor(Date.now() / 1000) } : null })) } : current);
      return;
    }
    try { await invoke("refresh_usage"); }
    catch (caught: unknown) { setError(errorMessage(caught)); }
  }

  function retryInitialRead() {
    void invoke<AppState>("get_state")
      .then((initial) => { setState(initial); setError(null); })
      .catch((caught: unknown) => setError(errorMessage(caught)));
  }

  function quit() {
    if (native) void invoke("quit_app").catch((caught: unknown) => setError(errorMessage(caught)));
  }

  const displayedProviders = state?.providers.filter((provider) => selection === "both" || selection === provider.id) ?? [];
  const refreshing = displayedProviders.some((provider) => provider.refreshing);

  return (
    <div ref={shell} className="popover-shell" data-layout={selection} data-theme={theme}>
      <header className="app-header">
        <div className="brand">
          <svg className="brand-mark" viewBox="0 0 30 18" fill="currentColor" aria-hidden="true">
            <path d="M.8 15.8 7.8 2.2 14.8 15.8Z M5.4 12.8 7.8 7.7 10.2 12.8Z" fillRule="evenodd" />
            <path d="M13.8 2.2h3.4l4.2 9.2 4.2-9.2H29l-6.1 13.6h-3Z" />
          </svg>
          <h1>Delta-V</h1>
        </div>
        <nav className="provider-picker" aria-label="Show providers">
          {(["claude", "codex", "both"] as const).map((value) => (
            <button
              key={value}
              aria-pressed={selection === value}
              className={selection === value ? "selected" : ""}
              onClick={() => void selectProviders(value)}
              disabled={!state || saving}
            >
              {value === "both" ? "Both" : providerNames[value]}
            </button>
          ))}
        </nav>
      </header>
      <main className="scroll-area">
        <div ref={content} className="scroll-content">
          {preview && <div className="preview-notice">Sample data · Browser preview</div>}
          {state?.paused && <div className="global-notice">Automatic refresh is paused while your screen is locked.</div>}
          {state?.settings_error && <div className="notice global-error" role="status">{state.settings_error}</div>}
          {error && <div className="notice global-error" role="alert">{error}</div>}
          {settingsOpen && state ? (
            <SettingsPanel key={selection} state={state} saving={saving} now={now} onSave={saveSettings} onClose={() => setSettingsOpen(false)} onThemePreview={setThemePreview} />
          ) : state ? (
            <>
              <div id="provider-limits" className={`provider-grid${selection === "both" ? " two-providers" : ""}`}>
                {displayedProviders.map((provider) => (
                  <ProviderColumn key={provider.id} provider={provider} settings={state.settings} now={now} expanded={expanded} />
                ))}
              </div>
              <button className="expand-button" onClick={() => setExpanded(!expanded)} aria-expanded={expanded} aria-controls="provider-limits">
                {expanded ? "Show less" : "Show more"}<Icon name="chevron" />
              </button>
            </>
          ) : (
            <div className="app-loading">
              <span className="loading-line" />
              <h2>{native ? "Reading your usage" : "Open Delta-V from your menu bar"}</h2>
              <p>{native ? "Connecting to your saved account information." : "Usage is available in the macOS app."}</p>
              {native && error && <button className="text-button" onClick={retryInitialRead}>Try again</button>}
            </div>
          )}
        </div>
      </main>
      <footer className="app-footer">
        <button
          className={`footer-button${settingsOpen ? " active" : ""}`}
          onClick={() => setSettingsOpen(!settingsOpen)}
          aria-expanded={settingsOpen}
          disabled={!state}
        >
          <Icon name="settings" />Settings
        </button>
        <div className="footer-right">
          <button className="footer-button" onClick={() => void refresh()} disabled={refreshing || (!native && !preview)}>
            <Icon name="refresh" spinning={refreshing} />{refreshing ? "Checking" : "Check now"}
          </button>
          <span className="footer-divider" />
          <button className="footer-button quit-button" onClick={quit} disabled={!native}>
            <Icon name="quit" />Quit
          </button>
        </div>
      </footer>
    </div>
  );
}
