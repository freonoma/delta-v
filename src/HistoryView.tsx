import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { HistoryChart } from "./HistoryChart";
import { getHistoryPreview } from "./history-preview";
import { wholePercent } from "./usage";
import { chartPercent } from "./history-chart";
import type { HistoryAccount, HistoryCatalog, HistoryQuery, HistoryRange, HistoryRequest, HistoryState, HistoryWindow, PercentageMode, ProviderId, ProviderSelection, ProviderState, Settings } from "./types";
import "./HistoryView.css";

const native = isTauri();
const preview = import.meta.env.DEV && !native;
const names: Record<ProviderId, string> = { claude: "Claude", codex: "Codex" };
type Period = Exclude<HistoryRange["kind"], "day">;
const periods: { kind: Period; label: string }[] = [
  { kind: "today", label: "Today" }, { kind: "days7", label: "7 days" },
  { kind: "days30", label: "30 days" }, { kind: "all_time", label: "All time" },
];

function message(error: unknown): string {
  return error instanceof Error ? error.message : typeof error === "string" ? error : "Could not read saved history. Try again.";
}

function windowKey(window: HistoryWindow): string {
  return JSON.stringify([window.key.id, window.key.window_seconds, window.key.provenance]);
}

function dateLabel(timestamp: number, timezone?: string): string {
  return new Date(timestamp * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric", ...(timezone ? { timeZone: timezone } : {}) });
}

function defaultWindow(account: HistoryAccount | undefined): HistoryWindow | undefined {
  const available = [...account?.windows ?? []].sort((left, right) => right.last_recorded_at - left.last_recorded_at);
  const main = available.filter((window) => account?.provider === "claude"
    ? window.key.id === "session" || window.key.id === "weekly" : window.key.id.startsWith("rate_limit:"));
  const windows = main.length > 0 ? main : available;
  return windows.find((window) => window.key.window_seconds === 18_000 && window.key.provenance === "official")
    ?? windows.find((window) => window.key.window_seconds === 604_800 && window.key.provenance === "official")
    ?? windows[0];
}

function windowLabel(window: HistoryWindow, windows: HistoryWindow[]): string {
  if (windows.filter((candidate) => candidate.label === window.label).length < 2) return window.label;
  const duration = window.key.window_seconds;
  const length = duration === null ? "unspecified window" : duration >= 86_400 ? `${duration / 86_400}d` : `${duration / 3600}h`;
  return `${window.label} (${length}, ${window.key.provenance.replaceAll("_", " ")})`;
}

function Observations({ result, mode }: { result: HistoryQuery; mode: PercentageMode }) {
  const { peak, days_with_readings: covered, days_in_range: total, days_below_threshold: low, threshold_remaining: threshold } = result.observations;
  if (!peak) return null;
  const value = wholePercent(chartPercent(peak.used_fraction, mode), mode);
  return (
    <dl className="history-observations" aria-label="Recorded observations">
      <div title={`Recorded ${new Date(peak.observed_at * 1000).toLocaleString(undefined, { timeZone: result.timezone })} (${result.timezone})`}>
        <dt>{mode === "remaining" ? "Lowest remaining" : "Highest usage"}</dt><dd>{value}%</dd>
      </div>
      <div><dt>Below {threshold}% remaining</dt><dd>{low} <span>{low === 1 ? "day" : "days"}</span></dd></div>
      <div><dt>Days with readings</dt><dd>{covered}<span> / {total}</span></dd></div>
    </dl>
  );
}

function ProviderHistory({ provider, catalog, revision, range, settings, enabled, active, suspended, onDay }: {
  provider: ProviderId; catalog: HistoryCatalog; revision: number; range: HistoryRange;
  settings: Settings; enabled: boolean; active: boolean; suspended: boolean; onDay: (date: string) => void;
}) {
  const [selection, setSelection] = useState("current");
  const [chosenWindow, setChosenWindow] = useState<{ account: string; key: string } | null>(null);
  const [response, setResponse] = useState<{ key: string; value: HistoryQuery | null; error: string | null } | null>(null);
  const accounts = catalog.accounts.filter((account) => account.provider === provider);
  const current = catalog.current_accounts.find((account) => account.provider === provider)?.account_key ?? null;
  const accountKey = selection === "current" ? current : selection;
  const account = accounts.find((candidate) => candidate.account_key === accountKey);
  const quota = chosenWindow?.account === accountKey
    ? account?.windows.find((window) => windowKey(window) === chosenWindow.key) : defaultWindow(account);
  const request: HistoryRequest | null = accountKey && quota ? { provider, account_key: accountKey, limit: quota.key, range } : null;
  const requestJson = JSON.stringify(request);
  const key = JSON.stringify([request, revision, settings.threshold]);

  useEffect(() => {
    if (!request || !active || suspended) return;
    let disposed = false;
    const read = preview ? Promise.resolve().then(() => getHistoryPreview().query(request, settings.threshold))
      : invoke<HistoryQuery>("query_history", { request });
    void read.then((value) => {
      if (!disposed) setResponse({ key, value, error: null });
    }).catch((error: unknown) => {
      if (!disposed) setResponse({ key, value: null, error: message(error) });
    });
    return () => { disposed = true; };
    // A response belongs to one exact account, quota, period, and store revision.
  }, [requestJson, key, settings.threshold, active, suspended]);

  const result = response?.key === key ? response.value : null;
  const failure = response?.key === key ? response.error : null;
  const loading = suspended || (request !== null && response?.key !== key);
  const source = quota?.key.provenance === "official" ? "Official readings"
    : quota?.key.provenance === "local_estimate" ? "Local estimates" : "Unknown source";
  const missingWindow = chosenWindow?.account === accountKey && !quota;

  return (
    <section hidden={!active} className={`history-provider ${provider}`} aria-label={`${names[provider]} history`} aria-busy={loading}>
      <div className="history-provider-heading">
        <h3><span className="history-provider-dot" />{names[provider]}</h3>
        {account && <label className="history-window-picker"><span className="history-sr-only">{names[provider]} usage window</span>
          <select value={quota ? windowKey(quota) : ""} onChange={(event) => setChosenWindow({ account: account.account_key, key: event.target.value })}>
            {missingWindow && <option value="" disabled>Window no longer saved</option>}
            {account.windows.map((window) => <option key={windowKey(window)} value={windowKey(window)}>{windowLabel(window, account.windows)}</option>)}
          </select>
        </label>}
      </div>
      <div className="history-account-row">
        {accounts.length > 1 || !current || !account || selection !== "current" ? (
          <label><span className="history-sr-only">{names[provider]} history account</span>
            <select value={selection} onChange={(event) => { setSelection(event.target.value); setChosenWindow(null); }}>
              <option value="current">{current ? "Current account" : "Current account not verified"}</option>
              {accounts.filter((saved) => saved.account_key !== current).map((saved, index) => (
                <option key={saved.account_key} value={saved.account_key}>Saved account {index + 1} · last reading {dateLabel(saved.last_recorded_at)}</option>
              ))}
              {selection !== "current" && selection === current && <option value={selection}>Saved account · currently connected</option>}
              {selection !== "current" && !account && <option value={selection}>Account no longer saved</option>}
            </select>
          </label>
        ) : <span>Current account</span>}
        {!enabled && <span>Disconnected · saved history</span>}
      </div>
      {loading ? <div className="history-chart-placeholder" role="status">Reading saved history</div>
        : failure ? <div className="history-chart-placeholder history-query-error" role="alert">{failure}</div>
        : !request ? <div className="history-chart-placeholder">
          <strong>{selection === "current" && !current ? "Current account not verified" : missingWindow ? "No saved readings for this window" : "No readings saved for this account"}</strong>
          <p>{selection === "current" && !current ? "Choose a saved account above to view its history. A successful usage check can identify the connected account."
            : missingWindow ? "Choose another saved window above." : "New readings will appear after a successful usage check with recording on."}</p>
        </div>
        : result && <>
          <HistoryChart key={requestJson} result={result} percentageMode={settings.percentage_mode} onSelectDay={onDay} />
          <div className="history-chart-source"><span>{source}</span><span>{result.timezone.replaceAll("_", " ")}</span></div>
          <Observations result={result} mode={settings.percentage_mode} />
        </>}
    </section>
  );
}

export function HistoryView({ settings, providers, active, onChange, onSettings, onClose }: {
  settings: Settings; providers: ProviderState[]; onChange: (state: HistoryState) => void;
  active: boolean; onSettings: () => void; onClose: () => void;
}) {
  const [selection, setSelection] = useState<ProviderSelection>(settings.providers);
  const [range, setRange] = useState<HistoryRange>({ kind: "days7" });
  const [previousPeriod, setPreviousPeriod] = useState<Period>("days7");
  const [state, setState] = useState<HistoryState | null>(null);
  const [catalog, setCatalog] = useState<HistoryCatalog | null>(null);
  const [revision, setRevision] = useState(0);
  const [loadedConnection, setLoadedConnection] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [failure, setFailure] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const generation = useRef(0);
  const alive = useRef(false);
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const title = useRef<HTMLHeadingElement>(null);
  const dayBack = useRef<HTMLButtonElement>(null);
  const connectionKey = JSON.stringify(providers.map((provider) => [provider.id, provider.stale, provider.error?.kind, provider.recovery, provider.snapshot?.fetched_at]));
  const connectionRef = useRef(connectionKey);
  connectionRef.current = connectionKey;

  const reload = useCallback(async () => {
    const version = ++generation.current;
    const connection = connectionRef.current;
    setLoading(true);
    setFailure(null);
    try {
      const [next, accounts] = preview
        ? [getHistoryPreview().state(settingsRef.current), getHistoryPreview().catalog()]
        : await Promise.all([invoke<HistoryState>("get_history_state"), invoke<HistoryCatalog>("get_history_catalog")]);
      if (!alive.current || version !== generation.current) return;
      setState(next);
      setCatalog(accounts);
      setRevision(version);
      setLoadedConnection(connection);
      onChange(next);
    } catch (error: unknown) {
      if (alive.current && version === generation.current) { setFailure(message(error)); setCatalog(null); }
    } finally {
      if (alive.current && version === generation.current) setLoading(false);
    }
  }, [onChange]);

  useEffect(() => {
    if (!active) { setLoading(true); setLoadedConnection(null); return; }
    alive.current = true;
    let disposed = false;
    let stop: (() => void) | undefined;
    title.current?.focus({ preventScroll: true });
    if (native) {
      void listen<HistoryState>("history-state", (event) => {
        if (disposed) return;
        setState(event.payload);
        onChange(event.payload);
        void reload();
      }).then((unlisten) => { if (disposed) unlisten(); else { stop = unlisten; void reload(); } })
        .catch((error: unknown) => { if (!disposed) { setFailure(message(error)); setLoading(false); } });
    } else void reload();
    return () => { disposed = true; alive.current = false; generation.current += 1; stop?.(); };
  }, [reload, onChange, active]);

  useEffect(() => { if (alive.current) void reload(); }, [connectionKey, reload]);

  useEffect(() => {
    if (range.kind === "day") dayBack.current?.focus();
  }, [range]);

  useEffect(() => {
    if (!active) return;
    const calendarKey = () => `${new Date().toDateString()}:${Intl.DateTimeFormat().resolvedOptions().timeZone}`;
    let previous = calendarKey();
    const timer = window.setInterval(() => {
      const next = calendarKey();
      if (next !== previous) { previous = next; void reload(); }
    }, 60_000);
    return () => window.clearInterval(timer);
  }, [active, reload]);

  async function startRecording() {
    if (pending) return;
    setPending(true);
    setFailure(null);
    try {
      let next: HistoryState;
      if (preview) { getHistoryPreview().setRecording(true); next = getHistoryPreview().state(settingsRef.current); }
      else next = await invoke<HistoryState>("set_history_recording", { enabled: true });
      if (alive.current) { setState(next); onChange(next); await reload(); title.current?.focus(); }
    } catch (error: unknown) {
      if (alive.current) setFailure(message(error));
    } finally { setPending(false); }
  }

  function inspectDay(date: string) {
    if (range.kind !== "day") setPreviousPeriod(range.kind);
    setRange({ kind: "day", date });
  }

  function leaveDay() {
    setRange({ kind: previousPeriod });
    title.current?.focus();
  }

  const empty = state !== null && state.info.records === 0 && !state.error;
  const daily = range.kind !== "today" && range.kind !== "day";
  return (
    <section className="history-view" aria-labelledby="history-title" onKeyDown={(event) => {
      if (event.key === "Escape" && range.kind === "day") { event.stopPropagation(); leaveDay(); }
    }}>
      <div className="history-titlebar">
        <div><button className="icon-button history-back" aria-label="Back to usage" title="Back to usage" onClick={onClose}>
          <svg className="icon" viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="m11 5-5 5 5 5M6 10h10" /></svg>
        </button><h2 ref={title} tabIndex={-1} id="history-title">Usage history</h2></div>
        <button className="text-button history-manage" onClick={onSettings}>Manage history</button>
      </div>
      <div className="history-filters">
        <nav className="history-segment" aria-label="History providers">{(["claude", "codex", "both"] as const).map((provider) => (
          <button key={provider} aria-pressed={selection === provider} onClick={() => setSelection(provider)}>{provider === "both" ? "Both" : names[provider]}</button>
        ))}</nav>
        <nav className="history-periods" aria-label="History period">{periods.map((period) => (
          <button key={period.kind} className={period.kind === "all_time" ? "history-all-time" : ""} aria-pressed={range.kind === period.kind}
            onClick={() => setRange({ kind: period.kind })}>{period.label}</button>
        ))}</nav>
      </div>
      {range.kind === "day" && <div className="history-day-navigation">
        <button ref={dayBack} className="text-button" onClick={leaveDay}>Back to {periods.find((period) => period.kind === previousPeriod)?.label.toLowerCase()}</button>
        <strong>{dateLabel(Date.parse(`${range.date}T12:00:00Z`) / 1000, "UTC")}</strong>
      </div>}
      <div className="history-status-row"><span><span className={`history-recording-dot${state?.recording ? " recording" : ""}`} />
        {state ? state.recording ? "Recording on this Mac" : "Recording paused" : "Local history"}</span>
        <button className="text-button" disabled={loading || pending} onClick={() => void reload()}>{loading ? "Reading files" : "Reload"}</button>
      </div>
      {(failure || state?.error) && <div className="notice history-read-error" role="alert"><p>{failure ?? state?.error}</p>
        <button className="text-button" disabled={loading} onClick={() => void reload()}>Try again</button></div>}
      {state?.provider_issues.map((issue) => <p className="history-provider-issue" role="status" key={issue.provider}>{names[issue.provider]}: {issue.message}</p>)}
      {!state?.recording && !empty && state && <div className="history-paused"><p>Saved readings are still available.{state.retention !== "forever" && ` Readings older than ${state.retention === "days30" ? "30" : "90"} days are still removed.`}</p>
        <button className="text-button" disabled={pending} onClick={() => void startRecording()}>{pending ? "Resuming" : "Resume recording"}</button></div>}
      {loading && !catalog ? <div className="history-empty" role="status"><h3>Reading saved history</h3></div>
        : empty ? <div className="history-empty">
          <svg viewBox="0 0 32 32" width="32" height="32" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="M5 5v22h23M9 21l6-8 5 3 7-9" /></svg>
          <h3>{state.recording ? "Waiting for the next reading" : "Save your usage history"}</h3>
          <p>{state.recording ? "Recording is on. Your first successful usage check will appear here. Earlier activity cannot be filled in."
            : "Keep usage readings on this Mac to see how your allowance changes. No conversations or sign-in tokens are saved."}</p>
          {!state.recording && <><button className="primary-button" disabled={pending} onClick={() => void startRecording()}>{pending ? "Starting" : "Start recording"}</button>
            <p className="history-retention-note">{state.retention === "forever" ? "Kept until you delete them. Change this in Manage history." : `Kept for ${state.retention === "days30" ? "30" : "90"} days. Older readings are removed automatically.`}</p></>}
        </div>
        : catalog && <div className="history-charts" aria-busy={loading}>
          {(["claude", "codex"] as const).map((provider) => <ProviderHistory key={provider} provider={provider} catalog={catalog}
            revision={loading ? -generation.current : revision} range={range} settings={settings}
            active={active && (selection === "both" || selection === provider)} suspended={loading || loadedConnection !== connectionKey}
            enabled={provider === "claude" ? settings.claude_enabled : settings.codex_enabled} onDay={inspectDay} />)}
          <p className="history-explanation">{daily ? "Each point is the closest recorded reading to the limit that day, not the amount used that day. Select a day for its readings."
            : "Lines stop at gaps, decreases and changed windows. A reset time alone does not create a reading."} Blank periods have no saved readings.</p>
          <p className="history-explanation">Observations describe saved readings only. They do not measure tokens spent, time worked or productivity.</p>
        </div>}
    </section>
  );
}
