import { StrictMode, useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { UsagePanel } from "../../src/App";
import { BrandMark } from "../../src/BrandMark";
import { SettingsWindow } from "../../src/SettingsWindow";
import { useAppController } from "../../src/useAppController";
import type { Theme, UiStyle } from "../../src/types";
import { sampleReadings } from "./sample-readings";
import type { Scenario } from "./sample-readings";
import "../../src/styles.css";
import "../../src/SettingsWindow.css";
import "../../src/glass.css";
import "./study.css";

function MascotStudy() {
  const controller = useAppController();
  const [style, setStyle] = useState<UiStyle>("glass");
  const [theme, setTheme] = useState<Theme>("dark");
  const [still, setStill] = useState(false);
  const [view, setView] = useState(() => ({
    scenario: "typical" as Scenario, sample: sampleReadings("typical"),
  }));
  const timer = useRef<number | undefined>(undefined);
  const stage = useRef<HTMLDivElement>(null);
  const [scale, setScale] = useState(1);

  useEffect(() => {
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setScale(Math.min(1, entry.contentRect.width / 560));
    });
    if (stage.current) observer.observe(stage.current);
    return () => { observer.disconnect(); window.clearTimeout(timer.current); };
  }, []);

  const base = controller.state;
  const state = useMemo(() => base && ({
    ...base, settings: { ...base.settings, ui_style: style, theme, claude_mascot: true, codex_mascot: true, mascot_animation: !still }, providers: view.sample.providers,
  }), [base, style, theme, still, view.sample.providers]);
  if (!state) return null;
  const panel = { ...controller, state, now: view.sample.now, refresh: refreshSample };

  async function refreshSample() {
    if (view.scenario === "before-reset") return;
    const now = Math.floor(Date.now() / 1000);
    setView((previous) => ({
      ...previous, scenario: previous.scenario === "stale" ? "typical" : previous.scenario,
      sample: { now, providers: previous.sample.providers.map((provider) => ({
        ...provider, stale: false, error: null, reading_id: `${provider.id}:check:${now}`,
        snapshot: provider.snapshot && { ...provider.snapshot, fetched_at: now },
      })) },
    }));
  }

  function choose(scenario: Scenario) {
    window.clearTimeout(timer.current);
    const sample = sampleReadings(scenario);
    setView((previous) => ({ ...previous, scenario, sample }));
  }

  function playReset() {
    window.clearTimeout(timer.current);
    const deadline = Math.floor(Date.now() / 1000);
    const before = sampleReadings("before-reset", deadline);
    setView((previous) => ({ ...previous, scenario: "before-reset", sample: before }));
    timer.current = window.setTimeout(() => {
      const sample = sampleReadings("refilled", deadline);
      setView({ scenario: "refilled", sample });
    }, 1050);
  }

  const resetSelected = view.scenario === "before-reset" || view.scenario === "refilled";
  return (
    <div className="mascot-study" data-theme={theme}>
      <header className="study-heading">
        <div className="study-brand"><BrandMark /> Delta-V</div>
        <h1>Panel mascots</h1>
        <p>Try Clawd and Codex with sample weekly allowances.</p>
      </header>
      <div className="reading-controls">
        <label htmlFor="sample-reading">Reading</label>
        <select id="sample-reading" value={resetSelected ? "reset" : view.scenario}
          onChange={(event) => event.target.value === "reset" ? playReset() : choose(event.target.value as Scenario)}>
          <option value="typical">Typical week</option>
          <option value="half">Half remaining</option>
          <option value="low">Low allowance</option>
          <option value="exhausted">Used up</option>
          <option value="stale">Stale reading</option>
          <option value="reset">Weekly reset</option>
        </select>
        <button type="button" className="play-reset" onClick={playReset}>
          <span aria-hidden="true">▸</span> {resetSelected ? "Replay reset" : "Play weekly reset"}
        </button>
      </div>
      <div className="study-stage" ref={stage}>
        <div className="study-panel" style={{ zoom: scale }}>
          <UsagePanel controller={panel} />
        </div>
      </div>
      <div className="study-options">
        <fieldset><legend>Panel style</legend>
          {(["glass", "legacy"] as const).map((value) => <label key={value}>
            <input type="radio" name="style" checked={style === value} onChange={() => setStyle(value)} />{value}
          </label>)}
        </fieldset>
        <fieldset><legend>Appearance</legend>
          {(["light", "dark"] as const).map((value) => <label key={value}>
            <input type="radio" name="theme" checked={theme === value} onChange={() => setTheme(value)} />{value}
          </label>)}
        </fieldset>
        <label className="still-poses"><input type="checkbox" checked={still} onChange={(event) => setStill(event.target.checked)} /> Still poses</label>
      </div>
      <p className="study-status" role="status">{view.scenario === "before-reset" ? "Last reading before the weekly reset."
        : view.scenario === "refilled" ? "New reading received. The weekly allowances have refilled." : ""}</p>
      <footer className="study-credit">Artwork from <a href="https://claude.ai/images/clawd/core/Clawd-Still.png">Anthropic</a> and{" "}
        <a href="https://github.com/openai/codex/blob/main/codex-rs/tui/src/pets/catalog.rs">OpenAI</a>.</footer>
    </div>
  );
}

const settingsOpen = new URLSearchParams(location.search).get("view") === "settings";
createRoot(document.getElementById("study")!).render(<StrictMode>
  {settingsOpen ? <SettingsWindow /> : <MascotStudy />}
</StrictMode>);
