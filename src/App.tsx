import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Amount, Limit, PercentageMode, ProviderId, ProviderState, ReconnectAction, RecoveryPhase, Settings } from "./types";
import { Startup } from "./Startup";
import { MiniView } from "./MiniView";
import { HistoryView } from "./HistoryView";
import { openSettings } from "./settings-navigation";
import { ProviderFeedback } from "./AccountControls";
import { Icon } from "./Icon";
import { BrandMark } from "./BrandMark";
import { clientNames, issueStatuses, providerEnabled, providerNames, recoveryMessages } from "./provider-display";
import { errorMessage, native, preview } from "./ui-state";
import { defaultPanelPreferences, useAppController } from "./useAppController";
import { compactLimits, featuredQuota, miniLimits, quotaPercent, remainingPercent, sampleAge, shortDuration, usedPercent, wholePercent } from "./usage";

const provenanceNames: Record<Limit["provenance"], string> = {
  official: "Official", local_estimate: "Local estimate", unknown: "Unknown source",
};

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
  const tracksThisProvider = settings.tracked_limit.startsWith(`${provider.id}:`);
  const featured = featuredQuota(provider.id, snapshot?.limits ?? [], settings.tracked_limit, now);
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

export default function App() {
  const {
    state, error, setError, now, login,
    panelPreferences, panelReady, panelBusy, panelCommandPending,
    saving, pendingRecovery, pendingConnection,
    selectProviders, changePanelPreferences,
    updateHistoryPreferences, refresh, reconnect, cancelReconnect, setProviderEnabled, retryInitialRead,
  } = useAppController();
  const [historyOpen, setHistoryOpen] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [showDragHint, setShowDragHint] = useState(false);
  const [keyboardNavigation, setKeyboardNavigation] = useState(false);
  const shell = useRef<HTMLDivElement>(null);
  const focusFrame = useRef(0);
  const content = useRef<HTMLDivElement>(null);
  const lastSize = useRef("");
  const selection = state?.settings.providers ?? "both";
  const theme = state?.settings.theme ?? "system";
  const { pinned, mini, expanded: miniExpanded, layout: miniLayout } = panelPreferences ?? defaultPanelPreferences;
  const miniActive = mini && pinned && !historyOpen;

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    let unlistenOpen: (() => void) | undefined;
    let unlistenMove: (() => void) | undefined;
    void getCurrentWindow().onMoved(() => {
      if (!disposed) setShowDragHint(false);
    }).then((stop) => { if (disposed) stop(); else unlistenMove = stop; })
      .catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    void listen("popover-reset", () => {
      if (!disposed) {
        setExpanded(false);
        setHistoryOpen(false);
        setShowDragHint(false);
        setKeyboardNavigation(false);
        window.cancelAnimationFrame(focusFrame.current);
        focusFrame.current = window.requestAnimationFrame(() => shell.current?.focus({ preventScroll: true }));
      }
    }).then((stop) => { if (disposed) stop(); else unlistenOpen = stop; }).catch((caught: unknown) => { if (!disposed) setError(errorMessage(caught)); });
    return () => { disposed = true; unlistenOpen?.(); unlistenMove?.(); window.cancelAnimationFrame(focusFrame.current); };
  }, [setError]);

  useEffect(() => {
    const element = shell.current;
    const scrollContent = content.current;
    if (!native || !panelReady || !element || !scrollContent) return;
    let frame = 0;
    const hasOpenDialog = () => document.querySelector("dialog[open]") !== null;
    let dialogOpen = hasOpenDialog();
    const resize = () => {
      window.cancelAnimationFrame(frame);
      frame = window.requestAnimationFrame(() => {
        // Background readings must not move the controls in an open dialog.
        if (hasOpenDialog()) return;
        const desiredWidth = miniActive ? selection === "both" && miniLayout === "columns" ? 336 : 232 : historyOpen || selection === "both" ? 560 : 340;
        const width = Math.min(desiredWidth, Math.max(220, window.screen.availWidth - 16));
        const ready = Math.abs(window.innerWidth - width) < 1;
        const chromeHeight = Array.from(element.children)
          .filter((child) => !child.classList.contains("scroll-area"))
          .reduce((height, child) => height + child.getBoundingClientRect().height, 2);
        // Measure after both widening and narrowing so restoration uses the final wrapping.
        const contentHeight = ready
          ? Math.ceil(chromeHeight + scrollContent.getBoundingClientRect().height)
          : window.innerHeight;
        const height = Math.min(620, Math.max(miniActive ? 100 : 180, contentHeight));
        const size = `${width}:${height}:${ready}`;
        if (lastSize.current === size) return;
        lastSize.current = size;
        void invoke("resize_popover", { width, height, ready }).catch((caught: unknown) => {
          if (lastSize.current === size) lastSize.current = "";
          setError(errorMessage(caught));
        });
      });
    };
    const observer = new ResizeObserver(resize);
    observer.observe(scrollContent);
    for (const child of element.children) {
      if (!child.classList.contains("scroll-area")) observer.observe(child);
    }
    const dialogs = new MutationObserver(() => {
      const open = hasOpenDialog();
      if (open === dialogOpen) return;
      dialogOpen = open;
      if (open) window.cancelAnimationFrame(frame);
      else resize();
    });
    dialogs.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["open"] });
    window.addEventListener("resize", resize);
    resize();
    return () => { observer.disconnect(); dialogs.disconnect(); window.removeEventListener("resize", resize); window.cancelAnimationFrame(frame); };
  }, [selection, miniActive, miniLayout, panelReady, historyOpen]);

  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      content.current?.parentElement?.scrollTo({ top: 0 });
    });
    return () => window.cancelAnimationFrame(frame);
  }, [historyOpen]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.metaKey && event.key === ",") {
        event.preventDefault();
        void openSettings().catch((caught: unknown) => setError(errorMessage(caught)));
        return;
      }
      if (event.key !== "Escape" || saving || panelCommandPending.current) return;
      if (historyOpen) { setHistoryOpen(false); return; }
      if (miniActive && miniExpanded) { void changePanelPreferences({ expanded: false }); return; }
      if (!miniActive && expanded) { setExpanded(false); return; }
      if (native) void invoke("hide_popover").catch((caught: unknown) => setError(errorMessage(caught)));
      else {
        setExpanded(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [historyOpen, expanded, miniActive, miniExpanded, saving, changePanelPreferences, panelCommandPending, setError]);

  async function togglePin() {
    if (panelBusy) return;
    const nextPinned = !pinned;
    if (await changePanelPreferences(nextPinned ? { pinned: true } : { pinned: false, mini: false, expanded: false })) {
      setShowDragHint(nextPinned);
    }
  }

  function quit() {
    if (native) void invoke("quit_app").catch((caught: unknown) => setError(errorMessage(caught)));
  }

  const displayedProviders = state?.providers.filter((provider) => selection === "both" || selection === provider.id) ?? [];
  const refreshing = displayedProviders.some((provider) => provider.refreshing);
  const recovering = state?.providers.some((provider) => provider.recovery !== null) || Object.keys(pendingRecovery).length > 0;
  const changingConnection = Object.keys(pendingConnection).length > 0;
  const hasConnectedProvider = state !== null && displayedProviders.some((provider) => providerEnabled(state.settings, provider.id));
  const hasMiniExtra = state !== null && displayedProviders.some((provider) =>
    providerEnabled(state.settings, provider.id) && provider.snapshot !== null
    && miniLimits(provider.id, provider.snapshot.limits,
      provider.id === "claude" ? state.settings.claude_windows : state.settings.codex_windows,
      state.settings.tracked_limit, now).length > 1,
  );

  async function openFullView(details = false) {
    if (!await changePanelPreferences({ mini: false })) return;
    setExpanded(details);
    window.requestAnimationFrame(() => shell.current?.focus({ preventScroll: true }));
  }

  async function openMiniView() {
    if (!await changePanelPreferences({ mini: true })) return;
    setHistoryOpen(false);
    setShowDragHint(false);
    window.requestAnimationFrame(() => shell.current?.focus({ preventScroll: true }));
  }

  return (
    <div ref={shell} className="popover-shell" data-layout={selection} data-theme={theme} data-pinned={pinned}
      data-mini={miniActive} data-mini-layout={miniLayout} data-history={historyOpen}
      tabIndex={-1} data-keyboard-navigation={keyboardNavigation}
      onPointerDownCapture={() => {
        window.cancelAnimationFrame(focusFrame.current);
        setKeyboardNavigation(false);
      }}
      onKeyDownCapture={(event) => {
        window.cancelAnimationFrame(focusFrame.current);
        if (event.key === "Tab") setKeyboardNavigation(true);
      }}>
      <header className="app-header" onMouseDown={(event) => {
        if (!native || !pinned || panelBusy || event.button !== 0
          || !(event.target instanceof Element) || event.target.closest("button, nav")) return;
        event.preventDefault();
        void invoke("drag_popover").catch((caught: unknown) => setError(errorMessage(caught)));
      }}>
        <div className="brand" title={pinned ? "Drag to move" : undefined} aria-label={miniActive ? "Delta-V" : undefined}>
          <BrandMark />
          {!miniActive && <h1>Delta-V</h1>}
          {miniActive && <span className="mini-mode">{state?.settings.percentage_mode}</span>}
        </div>
        <div className="header-actions">
          {!miniActive && !historyOpen && <nav className="provider-picker" aria-label="Show providers">
            {(["claude", "codex", "both"] as const).map((value) => (
              <button
                key={value}
                aria-pressed={selection === value}
                className={selection === value ? "selected" : ""}
                onClick={() => void selectProviders(value)}
                disabled={panelBusy || saving}
              >
                {value === "both" ? "Both" : providerNames[value]}
              </button>
            ))}
          </nav>}
          {miniActive && (hasMiniExtra || miniExpanded) && (
            <button className="pin-button" aria-label={miniExpanded ? "Hide extra limits" : "Show more limits"}
              title={miniExpanded ? "Hide extra limits" : "Show more limits"}
              aria-expanded={miniExpanded} aria-controls="mini-limits" disabled={panelBusy}
              onClick={() => void changePanelPreferences({ expanded: !miniExpanded })}>
              <Icon name={miniExpanded ? "minus" : "plus"} />
            </button>
          )}
          <button className="pin-button" aria-pressed={pinned} aria-label={pinned ? "Unpin panel" : "Pin panel"}
            title={pinned ? "Unpin and return to the menu bar" : "Keep open above other windows"}
            disabled={panelBusy || saving || (!native && !preview)} onClick={() => void togglePin()}>
            <Icon name="pin" />
          </button>
          {pinned && <button className="pin-button" aria-label={miniActive ? "Open full view" : "Switch to mini view"}
            title={miniActive ? "Open full view" : "Switch to mini view"} disabled={panelBusy || saving}
            onClick={() => {
              if (miniActive) void openFullView();
              else void openMiniView();
            }}>
            <Icon name={miniActive ? "expand" : "mini"} />
          </button>}
        </div>
        {pinned && showDragHint && (
          <div className="drag-hint">
            <p role="status">Drag the header to move</p>
            <button aria-label="Dismiss drag hint" title="Dismiss" onClick={() => setShowDragHint(false)}>
              <Icon name="close" />
            </button>
          </div>
        )}
      </header>
      <main className="scroll-area">
        <div ref={content} className="scroll-content">
          {preview && <div className="preview-notice">Sample data · Browser preview</div>}
          {state?.paused && <div className="global-notice">Automatic refresh is paused while your screen is locked.</div>}
          {state?.settings_error && <div className="notice global-error" role="status">{state.settings_error}</div>}
          {error && <div className="notice global-error" role="alert">{error}</div>}
          {panelReady && historyOpen && state && <div>
            <HistoryView settings={state.settings} providers={state.providers} active onChange={updateHistoryPreferences}
              onSettings={() => void openSettings("history").catch((caught: unknown) => setError(errorMessage(caught)))} onClose={() => setHistoryOpen(false)} />
          </div>}
          {panelReady && miniActive && state ? (
            <MiniView providers={displayedProviders} settings={state.settings} now={now} expanded={miniExpanded}
              pending={panelBusy} pendingRecovery={pendingRecovery}
              onExpand={() => void changePanelPreferences({ expanded: true })} onDetails={() => void openFullView(true)} />
          ) : panelReady && historyOpen ? null : panelReady && state ? (
            <>
              <Startup login={login} mode="prompt" dismissed={state.settings.launch_at_login_prompt_dismissed} />
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
              <button className="expand-button" disabled={panelBusy} onClick={() => setExpanded(!expanded)} aria-expanded={expanded} aria-controls="provider-limits">
                {expanded ? "Show less" : "Show more"}<Icon name="chevron" />
              </button>
            </>
          ) : (
            <div className="app-loading">
              <span className="loading-line" />
              <h2>{native ? "Reading your usage" : "Open Delta-V from your menu bar"}</h2>
              <p>{native ? "Connecting to your saved account information." : "Usage is available in the macOS app."}</p>
              {native && error && <button className="text-button" onClick={() => void retryInitialRead()}>Try again</button>}
            </div>
          )}
        </div>
      </main>
      {!miniActive && <footer className="app-footer">
        <div className="footer-left">
        <button
          className="footer-button"
          onClick={() => void openSettings().catch((caught: unknown) => setError(errorMessage(caught)))}
          title="Open Settings (Cmd+,)"
          disabled={panelBusy || saving}
        >
          <Icon name="settings" />Settings
        </button>
        <button className={`footer-button${historyOpen ? " active" : ""}`}
          aria-expanded={historyOpen} disabled={panelBusy || saving}
          onClick={() => setHistoryOpen(!historyOpen)}>
          <Icon name="history" />History
        </button>
        </div>
        <div className="footer-right">
          {!historyOpen && <><button className="footer-button" onClick={() => void refresh()} disabled={panelBusy || refreshing || recovering || changingConnection || !hasConnectedProvider || (!native && !preview)}>
            <Icon name="refresh" spinning={refreshing} />{refreshing ? "Checking" : "Check now"}
          </button>
          <span className="footer-divider" /></>}
          <button className="footer-button quit-button" onClick={quit} disabled={!native}>
            <Icon name="quit" />Quit
          </button>
        </div>
      </footer>}
    </div>
  );
}
