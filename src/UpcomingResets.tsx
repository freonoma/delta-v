import type { RefObject } from "react";
import type { UpcomingReset } from "./upcoming-resets";
import { providerNames } from "./provider-display";
import { sampleAge, shortDuration } from "./usage";
import { Icon } from "./Icon";

function ResetRow({ row, now }: { row: UpcomingReset; now: number }) {
  const date = row.resetAt !== null && (row.state === "upcoming" || row.state === "elapsed")
    ? new Date(row.resetAt * 1000) : null;
  const status = row.state === "upcoming" && row.resetAt !== null ? `In ${shortDuration(row.resetAt - now)}`
    : row.state === "elapsed" ? "Awaiting update"
    : row.state === "missing" ? "No reset time reported" : "Reset time unavailable";
  return (
    <li className={`upcoming-reset-row ${row.provider}`} data-reset-state={row.state}>
      <div className="upcoming-reset-heading">
        <span className="upcoming-reset-name"><strong>{providerNames[row.provider]}</strong> · {row.label}</span>
        <span className="upcoming-reset-status">{status}</span>
      </div>
      {date && <time dateTime={date.toISOString()} className="upcoming-reset-date" title={date.toLocaleString()}>
        {date.toLocaleString(undefined, {
          year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit", timeZoneName: "short",
        })}
      </time>}
      {row.stale && <p className="upcoming-reset-stale">Stale · Last reading {sampleAge(row.fetchedAt, now)}</p>}
    </li>
  );
}

export function UpcomingResets({ rows, now, open, onToggle, buttonRef }: {
  rows: UpcomingReset[]; now: number; open: boolean; onToggle: () => void;
  buttonRef: RefObject<HTMLButtonElement | null>;
}) {
  return (
    <section className="upcoming-resets" aria-labelledby="upcoming-resets-heading">
      <h2 id="upcoming-resets-heading">
        <button ref={buttonRef} className="upcoming-resets-toggle" onClick={onToggle}
          aria-expanded={open} aria-controls="upcoming-resets-content">
          <Icon name="clock" /><span>Upcoming resets</span>
          <span className="upcoming-resets-count" aria-hidden="true">{rows.length} {rows.length === 1 ? "window" : "windows"}</span>
          <Icon name="chevron" />
        </button>
      </h2>
      <div id="upcoming-resets-content" hidden={!open}>
        <p className="upcoming-resets-note">Reported times are shown in your local time zone. A fresh usage reading is needed to confirm allowance has returned.</p>
        {rows.length > 0 ? <ul className="upcoming-resets-list" aria-label="Quota reset times">
          {rows.map((row, index) => <ResetRow key={`${row.provider}:${row.limitId}:${row.windowSeconds}:${index}`} row={row} now={now} />)}
        </ul> : <p className="upcoming-resets-empty">No quota reset information is available for the selected providers.</p>}
      </div>
    </section>
  );
}
