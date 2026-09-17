import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AppState, Amount, IssueKind, Limit, PercentageMode, ProviderId, ProviderSelection, ProviderState, ReconnectAction, RecoveryPhase, Settings, Theme } from "./types";
import { createPreview } from "./preview";

const native = isTauri();
const preview = import.meta.env.DEV && !native;
const providerNames: Record<ProviderId, string> = { claude: "Claude", codex: "Codex" };
const clientNames: Record<ProviderId, string> = { claude: "Claude Code", codex: "Codex" };
const setupUrls: Record<ProviderId, string> = {
  claude: "https://code.claude.com/docs/en/quickstart",
  codex: "https://learn.chatgpt.com/docs/codex/cli",
};
const issueStatuses: Record<IssueKind, string> = {
  sign_in: "Sign in needed", authentication: "Sign in needed", recovery: "Sign in needed",
  credential_access: "Access needed", configuration: "Setup needed", client_missing: "Setup needed",
  access_denied: "Access denied", network: "Unavailable", rate_limited: "Waiting",
  service: "Unavailable", response: "Unavailable",
};
const recoveryMessages: Record<RecoveryPhase, string> = {
  renewing: "Reconnecting", signing_in: "Finish signing in in your browser",
  checking: "Checking usage", cancelling: "Cancelling",
};
const provenanceNames: Record<Limit["provenance"], string> = {
  official: "Official", local_estimate: "Local estimate", unknown: "Unknown source",
};

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  return "Something went wrong. Try refreshing.";
}

function providerEnabled(settings: Settings, provider: ProviderId): boolean {
  return provider === "claude" ? settings.claude_enabled : settings.codex_enabled;
}

function signInBlocked(provider: ProviderState, now: number): boolean {
  const kind = provider.error?.kind;
  const signInIssue = kind === "authentication" || kind === "sign_in" || kind === "recovery" || kind === "client_missing";
  return provider.refreshing || provider.recovery !== null
    || (kind !== undefined && !signInIssue)
    || (provider.next_retry_at !== null && provider.next_retry_at > now && !signInIssue);
}

function SetupInstructions({ provider }: { provider: ProviderId }) {
  const [opening, setOpening] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  async function open() {
    if (opening) return;
    setOpening(true);
    setFailure(null);
    try { await invoke("open_setup_instructions", { provider }); }
    catch (caught: unknown) { setFailure(errorMessage(caught)); }
    finally { setOpening(false); }
  }

  return (
    <div className="setup-instructions">
      <a className="setup-link" href={setupUrls[provider]} target="_blank" rel="noopener noreferrer"
        aria-label={`${clientNames[provider]} setup instructions (opens in browser)`} aria-disabled={opening}
        onClick={(event) => {
          if (native) {
            event.preventDefault();
            void open();
          }
        }}>
        {opening ? "Opening instructions" : `${clientNames[provider]} setup`}<Icon name="external" />
      </a>
      {failure && <p className="setup-error" role="alert">{failure}</p>}
    </div>
  );
}

function SignInConfirmation({ provider, disabled, onContinue, onCancel }: {
  provider: ProviderId; disabled: boolean; onContinue: () => void; onCancel: () => void;
}) {
  return (
    <div className="sign-in-confirmation" role="group" aria-label={`Sign in with ${clientNames[provider]}`}>
      <p>This opens {clientNames[provider]}’s sign-in flow. Choosing another account also changes the account saved by {clientNames[provider]} on this Mac.</p>
      <div className="recovery-actions">
        <button className="primary-button" onClick={onContinue} disabled={disabled}>Continue</button>
        <button className="text-button" onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}

function DisconnectConfirmation({ provider, onConfirm, onCancel }: {
  provider: ProviderId; onConfirm: () => void; onCancel: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const element = dialog.current;
    if (element && !element.open) element.showModal();
  }, []);

  return (
    <dialog ref={dialog} className="disconnect-dialog" aria-labelledby={`${provider}-disconnect-title`}
      aria-describedby={`${provider}-disconnect-description`} onClose={onCancel}
      onKeyDown={(event) => { if (event.key === "Escape") event.stopPropagation(); }}>
      <h3 id={`${provider}-disconnect-title`}>Disconnect {providerNames[provider]} from Delta-V?</h3>
      <p id={`${provider}-disconnect-description`}>Usage checks will stop and the current reading will be cleared. {clientNames[provider]} will stay signed in. You can reconnect at any time.</p>
      <div className="dialog-actions">
        <button className="text-button" autoFocus onClick={() => dialog.current?.close()}>Cancel</button>
        <button className="primary-button" onClick={() => {
          dialog.current?.close();
          onConfirm();
        }}>Disconnect</button>
      </div>
    </dialog>
  );
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

function Icon({ name, spinning = false }: { name: "refresh" | "settings" | "quit" | "clock" | "close" | "chevron" | "external"; spinning?: boolean }) {
  return (
    <svg className={spinning ? "icon spinning" : "icon"} viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {name === "refresh" && <><path d="M16.4 7A6.5 6.5 0 0 0 5 4.8L2.8 7M3.6 13A6.5 6.5 0 0 0 15 15.2l2.2-2.2" /><path d="M2.8 3.2V7h3.8m10.6 9.8V13h-3.8" /></>}
      {name === "settings" && <><path d="M4 3v14m6-14v14m6-14v14" /><path d="M2 7h4m2 6h4m2-8h4" strokeWidth="3" /></>}
      {name === "quit" && <><path d="M10 2.5v7M6 4.5a6.5 6.5 0 1 0 8 0" /></>}
      {name === "clock" && <><circle cx="10" cy="10" r="6.5" /><path d="M10 6v4l2.6 1.5" /></>}
      {name === "close" && <path d="m5 5 10 10M15 5 5 15" />}
      {name === "chevron" && <path d="m6 8 4 4 4-4" />}
      {name === "external" && <><path d="M11 3h6v6m0-6L8 12" /><path d="M8 4H4a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1v-4" /></>}
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

function ProviderFeedback({ provider, recovery, now, onReconnect, onCancel, showActions = true }: {
  provider: ProviderState;
  recovery: RecoveryPhase | null;
  now: number;
  onReconnect: (provider: ProviderId, action: ReconnectAction) => void;
  onCancel: (provider: ProviderId) => void;
  showActions?: boolean;
}) {
  const [confirmSignIn, setConfirmSignIn] = useState(false);
  useEffect(() => { if (recovery) setConfirmSignIn(false); }, [recovery]);
  if (recovery) {
    return (
      <div className="recovery-notice" role="status">
        <p className="recovery-progress"><Icon name="refresh" spinning />{recoveryMessages[recovery]}</p>
        {recovery === "signing_in" && <p className="recovery-help">Return here after finishing with {clientNames[provider.id]}.</p>}
        {recovery !== "checking" && (
          <button className="text-button" onClick={() => onCancel(provider.id)} disabled={recovery === "cancelling"}>Cancel</button>
        )}
      </div>
    );
  }
  const issue = provider.error;
  if (!issue) return null;
  const canReconnect = issue.kind === "authentication" || issue.kind === "recovery";
  const needsSignIn = issue.kind === "sign_in" || issue.kind === "client_missing";
  let help: string | null = null;
  if (issue.kind === "sign_in") {
    help = provider.id === "claude"
      ? "Sign in with your Claude subscription. Delta-V needs the standalone Claude Code terminal app. Claude Desktop alone does not connect it."
      : "Sign in with your ChatGPT account. Delta-V can use the Codex client included with the ChatGPT or Codex Mac app, or a separate Codex CLI installation.";
  } else if (issue.kind === "client_missing") {
    help = provider.id === "claude"
      ? "Install or update the standalone Claude Code terminal app, then sign in here. Claude Desktop alone does not connect Delta-V."
      : "Update the ChatGPT or Codex Mac app, or install Codex CLI, then return here to sign in with your ChatGPT account.";
  } else if (canReconnect) {
    help = `Reconnect tries your saved ${clientNames[provider.id]} sign-in. Sign in again to connect another account.`;
  } else if (issue.kind === "credential_access") {
    help = "Check file and Keychain access for the CLI’s saved sign-in. The README lists the locations Delta-V reads.";
  } else if (issue.kind === "configuration") {
    help = "See “Connect your accounts” in the README for setup steps.";
  }
  return (
    <div className={`notice error-notice${needsSignIn ? " setup-notice" : ""}`} role="status">
      <p>{issue.message}</p>
      {help && <p className="recovery-help">{help}</p>}
      {(needsSignIn || canReconnect) && <SetupInstructions provider={provider.id} />}
      {showActions && (needsSignIn || canReconnect) && !confirmSignIn && (
        <div className="recovery-actions">
          <button className="primary-button" disabled={signInBlocked(provider, now)} onClick={() => needsSignIn ? setConfirmSignIn(true) : onReconnect(provider.id, "renew")}>
            {needsSignIn ? `Sign in with ${clientNames[provider.id]}` : "Reconnect"}
          </button>
          {canReconnect && <button className="text-button" disabled={signInBlocked(provider, now)} onClick={() => setConfirmSignIn(true)}>Sign in again</button>}
        </div>
      )}
      {showActions && confirmSignIn && <SignInConfirmation provider={provider.id} disabled={signInBlocked(provider, now)} onContinue={() => {
        setConfirmSignIn(false);
        onReconnect(provider.id, "sign_in");
      }} onCancel={() => setConfirmSignIn(false)} />}
      {provider.next_retry_at !== null && provider.next_retry_at > now && (
        <p className="retry-time">{needsSignIn || canReconnect ? "Next check" : "Retry"} in {shortDuration(provider.next_retry_at - now)}</p>
      )}
    </div>
  );
}

function ProviderColumn({ provider, settings, now, expanded, pendingRecovery, connectionPending, onReconnect, onCancel, onConnect }: {
  provider: ProviderState;
  settings: Settings;
  now: number;
  expanded: boolean;
  pendingRecovery: RecoveryPhase | null;
  connectionPending: boolean;
  onReconnect: (provider: ProviderId, action: ReconnectAction) => void;
  onCancel: (provider: ProviderId) => void;
  onConnect: (provider: ProviderId) => void;
}) {
  const enabled = providerEnabled(settings, provider.id);
  const snapshot = enabled ? provider.snapshot : null;
  const recovery = pendingRecovery === "cancelling" ? pendingRecovery : provider.recovery ?? pendingRecovery;
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
  const busy = provider.refreshing || recovery !== null;
  const status = recovery ? recovery === "signing_in" ? "Signing in" : recoveryMessages[recovery]
    : !enabled ? "Disconnected"
    : provider.refreshing ? snapshot ? "Refreshing" : "Loading"
    : provider.error ? issueStatuses[provider.error.kind]
    : !snapshot ? "Loading" : stale ? "Stale" : "Updated";
  const emptyHeadline = snapshot
    ? expired ? "Waiting for fresh usage" : tracksThisProvider ? "Tracked window unavailable" : "No quota reading"
    : "Reading usage";
  const emptyDescription = snapshot
    ? expired ? "A quota window has reached its reset time" : tracksThisProvider ? "Choose another window in Settings" : "The provider has no usable quota percentage"
    : "Checking your account limits";
  return (
    <section className={`provider-column ${provider.id}`} aria-label={`${providerNames[provider.id]} usage`} aria-busy={busy}>
      <div className="provider-heading">
        <div className="provider-title">
          <h2>{providerNames[provider.id]}</h2>
          {expanded && snapshot?.plan && <span className="plan-label">{snapshot.plan}</span>}
        </div>
        <span className={`connection-status${snapshot && stale ? " stale" : ""}${busy ? " loading" : ""}${!snapshot ? " disconnected" : ""}`} title={snapshot ? `Updated ${sampleAge(snapshot.fetched_at, now)}` : status}>
          <span className="status-dot" />{status}
        </span>
      </div>
      {!enabled && !recovery && <div className="disconnected-state">
        <p>Usage checks are off. {clientNames[provider.id]} stays signed in.</p>
        <button className="primary-button" aria-label={`Connect ${providerNames[provider.id]}`} disabled={connectionPending} onClick={() => onConnect(provider.id)}>{connectionPending ? "Connecting" : "Connect"}</button>
      </div>}
      {enabled && (snapshot || (!provider.error && !recovery)) && <div className={`remaining-summary${low ? " low" : ""}`}>
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
      </div>}
      {(enabled || recovery) && <ProviderFeedback provider={provider} recovery={recovery} now={now} onReconnect={onReconnect} onCancel={onCancel} />}
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
      ) : snapshot && !provider.error && !recovery && !missingSelected && (
        <div className="empty-state">
          <span className="empty-rule" />
          <p>{provider.refreshing ? "Your limits will appear here." : "No quota windows are available."}</p>
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

function AccountRow({ provider, enabled, pending, recovery, now, paused, onSetEnabled, onReconnect, onCancel }: {
  provider: ProviderState;
  enabled: boolean;
  pending: boolean;
  recovery: RecoveryPhase | null;
  now: number;
  paused: boolean;
  onSetEnabled: (provider: ProviderId, enabled: boolean) => void;
  onReconnect: (provider: ProviderId, action: ReconnectAction) => void;
  onCancel: (provider: ProviderId) => void;
}) {
  const [confirmSignIn, setConfirmSignIn] = useState(false);
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);
  useEffect(() => { if (!enabled || recovery) setConfirmSignIn(false); }, [enabled, recovery]);
  useEffect(() => { if (!enabled) setConfirmDisconnect(false); }, [enabled]);
  const status = !enabled ? "Disconnected from Delta-V" : recovery ? recoveryMessages[recovery]
    : provider.refreshing ? "Checking usage" : provider.error ? issueStatuses[provider.error.kind]
    : provider.snapshot ? "Connected" : "Ready to check";
  const canReconnect = provider.error?.kind === "authentication" || provider.error?.kind === "recovery";
  const needsSignIn = provider.error?.kind === "sign_in" || provider.error?.kind === "client_missing";
  const signInDisabled = paused || pending || recovery !== null || signInBlocked(provider, now);
  return (
    <div className="account-row" role="group" aria-label={`${providerNames[provider.id]} connection`} aria-busy={pending || recovery !== null}>
      <div className="account-heading">
        <div><h4>{providerNames[provider.id]}</h4><p>{status}</p></div>
        <button className="text-button" aria-label={`${enabled ? "Disconnect" : "Connect"} ${providerNames[provider.id]}${enabled ? " from Delta-V" : ""}`} disabled={pending || (!enabled && recovery !== null)} onClick={() => {
          if (enabled) {
            setConfirmSignIn(false);
            setConfirmDisconnect(true);
          } else onSetEnabled(provider.id, true);
        }}>
          {pending ? enabled ? "Disconnecting" : "Connecting" : enabled ? "Disconnect" : "Connect"}
        </button>
      </div>
      {(enabled || recovery) && <ProviderFeedback provider={provider} recovery={recovery} now={now} onReconnect={onReconnect} onCancel={onCancel} showActions={false} />}
      {enabled && !recovery && !confirmSignIn && <div className="account-actions">
        {canReconnect && <button className="text-button" disabled={signInDisabled} onClick={() => onReconnect(provider.id, "renew")}>Reconnect</button>}
        <button className="text-button" disabled={signInDisabled} onClick={() => setConfirmSignIn(true)}>{needsSignIn ? `Sign in with ${clientNames[provider.id]}` : "Sign in again"}</button>
      </div>}
      {enabled && confirmSignIn && <SignInConfirmation provider={provider.id} disabled={signInDisabled} onContinue={() => {
        setConfirmSignIn(false);
        onReconnect(provider.id, "sign_in");
      }} onCancel={() => setConfirmSignIn(false)} />}
      {enabled && confirmDisconnect && <DisconnectConfirmation provider={provider.id} onConfirm={() => {
        setConfirmDisconnect(false);
        onSetEnabled(provider.id, false);
      }} onCancel={() => setConfirmDisconnect(false)} />}
    </div>
  );
}

function SettingsPanel({ state, saving, now, pendingRecovery, pendingConnection, onSave, onClose, onThemePreview, onSetEnabled, onReconnect, onCancel }: {
  state: AppState;
  saving: boolean;
  now: number;
  pendingRecovery: Partial<Record<ProviderId, RecoveryPhase>>;
  pendingConnection: Partial<Record<ProviderId, boolean>>;
  onSave: (settings: Settings) => Promise<void>;
  onClose: () => void;
  onThemePreview: (theme: Theme | null) => void;
  onSetEnabled: (provider: ProviderId, enabled: boolean) => void;
  onReconnect: (provider: ProviderId, action: ReconnectAction) => void;
  onCancel: (provider: ProviderId) => void;
}) {
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
      await onSave({
        ...draft, providers: state.settings.providers, claude_enabled: state.settings.claude_enabled,
        codex_enabled: state.settings.codex_enabled, tracked_limit: tracked, threshold: parsedThreshold,
        refresh_seconds: parsedInterval,
      });
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
      <section className="accounts-settings" aria-label="Accounts">
        <h3>Accounts</h3>
        <p>Changes here apply immediately. Disconnect stops usage checks in Delta-V. It does not sign you out of Claude Code or Codex.</p>
        <div className="accounts-list">
          {state.providers.map((provider) => (
            <AccountRow key={provider.id} provider={provider} enabled={providerEnabled(state.settings, provider.id)}
              pending={pendingConnection[provider.id] ?? false} now={now} paused={state.paused}
              recovery={pendingRecovery[provider.id] === "cancelling" ? "cancelling" : provider.recovery ?? pendingRecovery[provider.id] ?? null}
              onSetEnabled={onSetEnabled} onReconnect={onReconnect} onCancel={onCancel} />
          ))}
        </div>
      </section>
    </section>
  );
}

export default function App() {
  const [state, setState] = useState<AppState | null>(() => preview ? createPreview(window.location.search) : null);
  const [error, setError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [themePreview, setThemePreview] = useState<Theme | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [keyboardNavigation, setKeyboardNavigation] = useState(false);
  const [pendingRecovery, setPendingRecovery] = useState<Partial<Record<ProviderId, RecoveryPhase>>>({});
  const [pendingConnection, setPendingConnection] = useState<Partial<Record<ProviderId, boolean>>>({});
  const [now, setNow] = useState(Math.floor(Date.now() / 1000));
  const shell = useRef<HTMLDivElement>(null);
  const focusFrame = useRef(0);
  const content = useRef<HTMLDivElement>(null);
  const lastSize = useRef("");
  const recoveryCommands = useRef(new Set<ProviderId>());
  const connectionCommands = useRef(new Set<ProviderId>());
  const previewTimers = useRef<Partial<Record<ProviderId, number>>>({});
  const selection = state?.settings.providers ?? "both";
  const theme: Theme = (settingsOpen ? themePreview : null) ?? state?.settings.theme ?? "system";

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000);
    const recoveryTimers = previewTimers.current;
    return () => {
      window.clearInterval(timer);
      for (const recoveryTimer of Object.values(recoveryTimers)) window.clearTimeout(recoveryTimer);
    };
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
        setKeyboardNavigation(false);
        window.cancelAnimationFrame(focusFrame.current);
        focusFrame.current = window.requestAnimationFrame(() => shell.current?.focus({ preventScroll: true }));
      }
    }).then((stop) => { if (disposed) stop(); else unlistenOpen = stop; }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    void listen<AppState>("usage-updated", (event) => {
      receivedEvent = true;
      if (!disposed) setState(event.payload);
    }).then(async (stop) => {
      if (disposed) { stop(); return; }
      unlisten = stop;
      const initial = await invoke<AppState>("get_state");
      if (!disposed && !receivedEvent) setState(initial);
    }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    return () => { disposed = true; unlisten?.(); unlistenOpen?.(); window.cancelAnimationFrame(focusFrame.current); };
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
      if (preview) setState((current) => current ? {
        ...current, settings: { ...settings, claude_enabled: current.settings.claude_enabled, codex_enabled: current.settings.codex_enabled },
      } : current);
      else {
        const saved = await invoke<AppState>("save_settings", { settings });
        setState((current) => current ? {
          ...current, settings: { ...saved.settings, claude_enabled: current.settings.claude_enabled, codex_enabled: current.settings.codex_enabled },
          settings_error: saved.settings_error,
        } : saved);
      }
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
    if (recoveryCommands.current.size > 0 || connectionCommands.current.size > 0 || state?.providers.some((provider) => provider.recovery !== null)) return;
    setError(null);
    if (preview) {
      setState((current) => current ? { ...current, providers: current.providers.map((provider) => ({ ...provider, snapshot: providerEnabled(current.settings, provider.id) && provider.snapshot ? { ...provider.snapshot, fetched_at: Math.floor(Date.now() / 1000) } : null })) } : current);
      return;
    }
    try { await invoke("refresh_usage"); }
    catch (caught: unknown) { setError(errorMessage(caught)); }
  }

  function setPreviewRecovery(providerId: ProviderId, recovery: RecoveryPhase | null) {
    setState((current) => current ? {
      ...current,
      providers: current.providers.map((provider) => provider.id === providerId ? { ...provider, recovery } : provider),
    } : current);
  }

  function simulateRecovery(providerId: ProviderId, action: ReconnectAction) {
    setState((current) => current ? {
      ...current, providers: current.providers.map((provider) => provider.id === providerId ? {
        ...provider, snapshot: null, error: null, recovery: action === "renew" ? "renewing" : "signing_in", stale: true,
      } : provider),
    } : current);
    previewTimers.current[providerId] = window.setTimeout(() => {
      setPreviewRecovery(providerId, "checking");
      previewTimers.current[providerId] = window.setTimeout(() => {
        const sample = createPreview().providers.find((provider) => provider.id === providerId);
        if (sample?.snapshot) {
          const connected = { ...sample, snapshot: { ...sample.snapshot, fetched_at: Math.floor(Date.now() / 1000) } };
          setState((current) => current && providerEnabled(current.settings, providerId) ? {
            ...current,
            providers: current.providers.map((provider) => provider.id === providerId ? connected : provider),
          } : current);
        }
        delete previewTimers.current[providerId];
      }, 900);
    }, action === "renew" ? 1500 : 5000);
  }

  function clearPendingRecovery(provider: ProviderId) {
    recoveryCommands.current.delete(provider);
    setPendingRecovery((current) => {
      const next = { ...current };
      delete next[provider];
      return next;
    });
  }

  async function reconnect(provider: ProviderId, action: ReconnectAction) {
    const current = state?.providers.find((candidate) => candidate.id === provider);
    if (!state || !current || !providerEnabled(state.settings, provider) || state.paused || current.recovery || current.refreshing
      || recoveryCommands.current.has(provider) || connectionCommands.current.has(provider)) return;
    if (action === "sign_in" && signInBlocked(current, now)) return;
    recoveryCommands.current.add(provider);
    setPendingRecovery((pending) => ({ ...pending, [provider]: action === "renew" ? "renewing" : "signing_in" }));
    setError(null);
    try {
      if (preview) simulateRecovery(provider, action);
      else if (native) await invoke("reconnect_provider", { provider, action });
    } catch (caught: unknown) {
      setError(errorMessage(caught));
    } finally {
      clearPendingRecovery(provider);
    }
  }

  async function cancelReconnect(provider: ProviderId) {
    const current = state?.providers.find((candidate) => candidate.id === provider);
    if (!current?.recovery || current.recovery === "checking" || current.recovery === "cancelling" || recoveryCommands.current.has(provider)) return;
    recoveryCommands.current.add(provider);
    setPendingRecovery((pending) => ({ ...pending, [provider]: "cancelling" }));
    setError(null);
    try {
      if (preview) {
        window.clearTimeout(previewTimers.current[provider]);
        setPreviewRecovery(provider, "cancelling");
        previewTimers.current[provider] = window.setTimeout(() => {
          setState((currentState) => currentState ? {
            ...currentState, providers: currentState.providers.map((candidate) => candidate.id === provider ? {
              ...candidate, recovery: null,
              error: providerEnabled(currentState.settings, provider) ? { kind: "recovery", message: "Sign-in was cancelled. Reconnect when you are ready." } : null,
            } : candidate),
          } : currentState);
          delete previewTimers.current[provider];
        }, 300);
      } else if (native) await invoke("cancel_reconnect", { provider });
    } catch (caught: unknown) {
      setError(errorMessage(caught));
    } finally {
      clearPendingRecovery(provider);
    }
  }

  async function setProviderEnabled(providerId: ProviderId, enabled: boolean) {
    if (!state || providerEnabled(state.settings, providerId) === enabled || connectionCommands.current.has(providerId)) return;
    const provider = state.providers.find((candidate) => candidate.id === providerId);
    if (!provider || (enabled && provider.recovery !== null)) return;
    connectionCommands.current.add(providerId);
    setPendingConnection((pending) => ({ ...pending, [providerId]: true }));
    setError(null);
    try {
      if (preview) {
        window.clearTimeout(previewTimers.current[providerId]);
        delete previewTimers.current[providerId];
        const sample = createPreview().providers.find((candidate) => candidate.id === providerId);
        const recovering = provider.recovery !== null;
        setState((current) => current ? {
          ...current,
          settings: { ...current.settings, [providerId === "claude" ? "claude_enabled" : "codex_enabled"]: enabled },
          providers: current.providers.map((candidate) => candidate.id !== providerId ? candidate : enabled && sample ? sample : {
            ...candidate, snapshot: null, error: null, refreshing: false, stale: true,
            next_retry_at: null, recovery: recovering ? "cancelling" : null,
          }),
        } : current);
        if (recovering) previewTimers.current[providerId] = window.setTimeout(() => {
          setPreviewRecovery(providerId, null);
          delete previewTimers.current[providerId];
        }, 300);
      } else if (native) await invoke("set_provider_enabled", { provider: providerId, enabled });
    } catch (caught: unknown) {
      setError(errorMessage(caught));
    } finally {
      connectionCommands.current.delete(providerId);
      setPendingConnection((pending) => {
        const next = { ...pending };
        delete next[providerId];
        return next;
      });
    }
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
  const recovering = state?.providers.some((provider) => provider.recovery !== null) || Object.keys(pendingRecovery).length > 0;
  const changingConnection = Object.keys(pendingConnection).length > 0;
  const hasConnectedProvider = state !== null && displayedProviders.some((provider) => providerEnabled(state.settings, provider.id));

  return (
    <div ref={shell} className="popover-shell" data-layout={selection} data-theme={theme}
      tabIndex={-1} data-keyboard-navigation={keyboardNavigation}
      onPointerDownCapture={() => {
        window.cancelAnimationFrame(focusFrame.current);
        setKeyboardNavigation(false);
      }}
      onKeyDownCapture={(event) => {
        window.cancelAnimationFrame(focusFrame.current);
        if (event.key === "Tab") setKeyboardNavigation(true);
      }}>
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
            <SettingsPanel key={selection} state={state} saving={saving} now={now} onSave={saveSettings}
              pendingRecovery={pendingRecovery} pendingConnection={pendingConnection}
              onClose={() => setSettingsOpen(false)} onThemePreview={setThemePreview}
              onSetEnabled={(id, enabled) => void setProviderEnabled(id, enabled)}
              onReconnect={(id, action) => void reconnect(id, action)} onCancel={(id) => void cancelReconnect(id)} />
          ) : state ? (
            <>
              <div id="provider-limits" className={`provider-grid${selection === "both" ? " two-providers" : ""}`}>
                {displayedProviders.map((provider) => (
                  <ProviderColumn
                    key={provider.id} provider={provider} settings={state.settings} now={now} expanded={expanded}
                    pendingRecovery={pendingRecovery[provider.id] ?? null}
                    connectionPending={pendingConnection[provider.id] ?? false}
                    onReconnect={(id, action) => void reconnect(id, action)} onCancel={(id) => void cancelReconnect(id)}
                    onConnect={(id) => void setProviderEnabled(id, true)}
                  />
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
          <button className="footer-button" onClick={() => void refresh()} disabled={refreshing || recovering || changingConnection || !hasConnectedProvider || (!native && !preview)}>
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
