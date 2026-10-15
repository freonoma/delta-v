import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppState, ProviderId, ProviderState, ReconnectAction, RecoveryPhase } from "./types";
import { Icon } from "./Icon";
import { clientNames, issueStatuses, providerEnabled, providerNames, recoveryMessages, signInBlocked } from "./provider-display";
import { errorMessage, native } from "./ui-state";
import { shortDuration } from "./usage";

const setupUrls: Record<ProviderId, string> = {
  claude: "https://code.claude.com/docs/en/quickstart",
  codex: "https://learn.chatgpt.com/docs/codex/cli",
};

function InstructionsLink({ href, label, accessibleLabel, onOpen }: {
  href: string; label: string; accessibleLabel: string; onOpen: () => Promise<void>;
}) {
  const [opening, setOpening] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const openingRef = useRef(false);

  async function open() {
    if (openingRef.current) return;
    openingRef.current = true;
    setOpening(true);
    setFailure(null);
    try { await onOpen(); }
    catch (caught: unknown) { setFailure(errorMessage(caught)); }
    finally { openingRef.current = false; setOpening(false); }
  }

  return (
    <div className="setup-instructions">
      <a className="setup-link" href={href} target="_blank" rel="noopener noreferrer"
        aria-label={`${accessibleLabel} (opens in browser)`} aria-disabled={opening}
        onClick={(event) => {
          if (native) {
            event.preventDefault();
            void open();
          }
        }}>
        {opening ? "Opening instructions" : label}<Icon name="external" />
      </a>
      {failure && <p className="setup-error" role="alert">{failure}</p>}
    </div>
  );
}

function SetupInstructions({ provider }: { provider: ProviderId }) {
  return <InstructionsLink href={setupUrls[provider]} label={`${clientNames[provider]} setup`}
    accessibleLabel={`${clientNames[provider]} setup instructions`}
    onOpen={() => invoke<void>("open_setup_instructions", { provider })} />;
}

function ConnectionHelp({ provider, topic }: { provider: ProviderId; topic: "credential_access" | "configuration" }) {
  const access = topic === "credential_access";
  const label = access ? "Credential access help" : "Connection setup help";
  const href = access ? "https://github.com/freonoma/delta-v#privacy" : "https://github.com/freonoma/delta-v#connect-your-accounts";
  return <InstructionsLink href={href} label={label} accessibleLabel={`${label} for ${providerNames[provider]}`}
    onOpen={() => invoke<void>("open_connection_help", { topic })} />;
}

function useSignInConfirmation() {
  const [confirmSignIn, setConfirmSignIn] = useState(false);
  const signInTrigger = useRef<HTMLButtonElement>(null);
  const focusFallback = useRef<HTMLDivElement>(null);
  const restoreFocus = useRef<"trigger" | "context" | null>(null);

  useLayoutEffect(() => {
    if (confirmSignIn || !restoreFocus.current) return;
    const target = restoreFocus.current;
    restoreFocus.current = null;
    const trigger = signInTrigger.current;
    if (target === "trigger" && trigger && !trigger.disabled) trigger.focus({ preventScroll: true });
    else focusFallback.current?.focus({ preventScroll: true });
  }, [confirmSignIn]);

  function cancelSignIn() {
    restoreFocus.current = "trigger";
    setConfirmSignIn(false);
  }

  function continueSignIn() {
    restoreFocus.current = "context";
    setConfirmSignIn(false);
  }

  return { confirmSignIn, setConfirmSignIn, signInTrigger, focusFallback, cancelSignIn, continueSignIn };
}

function SignInConfirmation({ provider, disabled, onContinue, onCancel }: {
  provider: ProviderId; disabled: boolean; onContinue: () => void; onCancel: () => void;
}) {
  const descriptionId = useId();
  return (
    <div className="sign-in-confirmation" role="group" aria-label={`Sign in with ${clientNames[provider]}`}
      aria-describedby={descriptionId}
      onKeyDown={(event) => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); onCancel(); } }}>
      <p id={descriptionId}>This opens {clientNames[provider]}’s sign-in flow. Choosing another account also changes the account saved by {clientNames[provider]} on this Mac.</p>
      <div className="recovery-actions">
        <button className="primary-button" onClick={onContinue} disabled={disabled}>Continue</button>
        <button className="text-button" autoFocus onClick={onCancel}>Cancel</button>
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

export function ProviderFeedback({ provider, recovery, now, onReconnect, onCancel, showActions = true }: {
  provider: ProviderState;
  recovery: RecoveryPhase | null;
  now: number;
  onReconnect: (provider: ProviderId, action: ReconnectAction) => void;
  onCancel: (provider: ProviderId) => void;
  showActions?: boolean;
}) {
  const { confirmSignIn, setConfirmSignIn, signInTrigger, focusFallback, cancelSignIn, continueSignIn } = useSignInConfirmation();
  useEffect(() => { if (recovery) setConfirmSignIn(false); }, [recovery]);
  if (recovery) {
    return (
      <div ref={focusFallback} tabIndex={-1} className="recovery-notice" role="status" aria-label={`${providerNames[provider.id]} connection`}>
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
    help = "Check file and Keychain access for the CLI’s saved sign-in. Expand the file and Keychain details in the help page.";
  } else if (issue.kind === "configuration") {
    help = "Review the connection setup steps and supported credential storage settings.";
  }
  return (
    <div ref={focusFallback} tabIndex={-1} className={`notice error-notice${needsSignIn ? " setup-notice" : ""}`} role="status">
      <p>{issue.message}</p>
      {help && <p className="recovery-help">{help}</p>}
      {(issue.kind === "credential_access" || issue.kind === "configuration") &&
        <ConnectionHelp key={`${provider.id}:${issue.kind}`} provider={provider.id} topic={issue.kind} />}
      {(needsSignIn || canReconnect) && <SetupInstructions provider={provider.id} />}
      {showActions && (needsSignIn || canReconnect) && !confirmSignIn && (
        <div className="recovery-actions">
          <button ref={needsSignIn ? signInTrigger : undefined} className="primary-button" disabled={signInBlocked(provider, now)} onClick={() => needsSignIn ? setConfirmSignIn(true) : onReconnect(provider.id, "renew")}>
            {needsSignIn ? `Sign in with ${clientNames[provider.id]}` : "Reconnect"}
          </button>
          {canReconnect && <button ref={signInTrigger} className="text-button" disabled={signInBlocked(provider, now)} onClick={() => setConfirmSignIn(true)}>Sign in again</button>}
        </div>
      )}
      {showActions && confirmSignIn && <SignInConfirmation provider={provider.id} disabled={signInBlocked(provider, now)} onContinue={() => {
        continueSignIn();
        onReconnect(provider.id, "sign_in");
      }} onCancel={cancelSignIn} />}
      {provider.next_retry_at !== null && provider.next_retry_at > now && (
        <p className="retry-time">{needsSignIn || canReconnect ? "Next check" : "Retry"} in {shortDuration(provider.next_retry_at - now)}</p>
      )}
    </div>
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
  const { confirmSignIn, setConfirmSignIn, signInTrigger, focusFallback, cancelSignIn, continueSignIn } = useSignInConfirmation();
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
    <div ref={focusFallback} tabIndex={-1} className="account-row" role="group" aria-label={`${providerNames[provider.id]} connection`} aria-busy={pending || recovery !== null}>
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
        <button ref={signInTrigger} className="text-button" disabled={signInDisabled} onClick={() => setConfirmSignIn(true)}>{needsSignIn ? `Sign in with ${clientNames[provider.id]}` : "Sign in again"}</button>
      </div>}
      {enabled && confirmSignIn && <SignInConfirmation provider={provider.id} disabled={signInDisabled} onContinue={() => {
        continueSignIn();
        onReconnect(provider.id, "sign_in");
      }} onCancel={cancelSignIn} />}
      {enabled && confirmDisconnect && <DisconnectConfirmation provider={provider.id} onConfirm={() => {
        setConfirmDisconnect(false);
        onSetEnabled(provider.id, false);
      }} onCancel={() => setConfirmDisconnect(false)} />}
    </div>
  );
}

export function AccountsSettings({ state, now, pendingRecovery, pendingConnection, onSetEnabled, onReconnect, onCancel }: {
  state: AppState;
  now: number;
  pendingRecovery: Partial<Record<ProviderId, RecoveryPhase>>;
  pendingConnection: Partial<Record<ProviderId, boolean>>;
  onSetEnabled: (provider: ProviderId, enabled: boolean) => void;
  onReconnect: (provider: ProviderId, action: ReconnectAction) => void;
  onCancel: (provider: ProviderId) => void;
}) {
  return (
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
  );
}
