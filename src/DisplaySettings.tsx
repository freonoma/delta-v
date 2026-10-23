import type { AppState, DisplayPreferences, MiniLayout, ProviderState, Theme } from "./types";
import type { SettingsDraftError } from "./settings-draft";
import { visibleTrackedLimit } from "./preference-state";
import { providerNames } from "./provider-display";
import { eligibleQuota } from "./usage";

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

export function DisplaySettings({ state, now, draft, setDraft, layoutDraft, setLayoutDraft, threshold, setThreshold, interval, setIntervalValue, busy, onThemePreview, invalidField, validationMessageId }: {
  state: AppState;
  now: number;
  draft: DisplayPreferences;
  setDraft: (preferences: DisplayPreferences) => void;
  layoutDraft: MiniLayout;
  setLayoutDraft: (layout: MiniLayout) => void;
  threshold: string;
  setThreshold: (value: string) => void;
  interval: string;
  setIntervalValue: (value: string) => void;
  busy: boolean;
  onThemePreview: (theme: Theme | null) => void;
  invalidField: SettingsDraftError["field"] | undefined;
  validationMessageId: string;
}) {
  const options = state.providers
    .filter((provider) => state.settings.providers === "both" || state.settings.providers === provider.id)
    .flatMap((provider) => (provider.snapshot?.limits ?? [])
      .filter((limit) => eligibleQuota(limit, now))
      .map((limit) => ({ value: `${provider.id}:${limit.id}`, label: `${providerNames[provider.id]} · ${limit.label}` })));
  const tracked = visibleTrackedLimit(draft.tracked_limit, state.settings.providers);
  const missingTracked = tracked !== "auto" && !options.some((option) => option.value === tracked);
  const thresholdInvalid = invalidField === "threshold";
  const intervalInvalid = invalidField === "interval";

  return (
    <>
      <fieldset className="style-picker">
        <legend>Interface style</legend>
        <p>Make Delta-V feel at home. Preview here, then save to apply.</p>
        <div className="style-picker-options">
          {(["glass", "legacy"] as const).map((style) => (
            <label key={style} className="style-option">
              <input type="radio" name="ui-style" value={style} checked={draft.ui_style === style}
                onChange={() => setDraft({ ...draft, ui_style: style })} />
              <span className={`style-swatch style-swatch-${style}`} aria-hidden="true">
                <span className="swatch-panel"><i /><span><b /><b /></span></span>
              </span>
              <span className="style-option-copy"><strong>{style === "glass" ? "Glass" : "Legacy"}</strong>
                <small>{style === "glass" ? "Soft light, layered surfaces" : "The original Delta-V look"}</small>
              </span>
            </label>
          ))}
        </div>
      </fieldset>
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
        <div className="window-picker-grid two-providers">
          {state.providers.map((provider) => {
            const key = provider.id === "claude" ? "claude_windows" : "codex_windows";
            return <WindowPicker key={provider.id} provider={provider} selected={draft[key]} onChange={(windows) => setDraft({ ...draft, [key]: windows })} />;
          })}
        </div>
      </div>
      <label className="setting-row">
        <span>Mini layout<small>When both providers are pinned</small></span>
        <select value={layoutDraft} disabled={busy} onChange={(event) => {
          const layout = event.target.value;
          if (layout === "columns" || layout === "stacked") setLayoutDraft(layout);
        }}>
          <option value="columns">Side by side</option>
          <option value="stacked">Stacked</option>
        </select>
      </label>
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
        <span>Low allowance threshold<small>Remaining percentage used for highlighting and enabled alerts. Save changes to apply.</small></span>
        <span className="number-field" data-invalid={thresholdInvalid || undefined}>
          <input name="threshold" type="number" min="0" max="100" step="1" inputMode="numeric" value={threshold}
            aria-invalid={thresholdInvalid || undefined} aria-describedby={thresholdInvalid ? validationMessageId : undefined}
            onChange={(event) => setThreshold(event.target.value)} />
          <span>%</span>
        </span>
      </label>
      <label className="setting-row">
        <span>Refresh interval<small>Claude checks at least 5 minutes apart. Service waits may be longer.</small></span>
        <span className="number-field" data-invalid={intervalInvalid || undefined}>
          <input name="interval" type="number" min="30" max="900" step="1" inputMode="numeric" value={interval}
            aria-invalid={intervalInvalid || undefined} aria-describedby={intervalInvalid ? validationMessageId : undefined}
            onChange={(event) => setIntervalValue(event.target.value)} />
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
    </>
  );
}
