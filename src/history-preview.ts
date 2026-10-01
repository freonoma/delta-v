import type {
  HistoryAccount, HistoryBreak, HistoryCatalog, HistoryDay, HistoryLimitKey, HistoryPoint,
  HistoryQuery, HistoryRequest, HistoryRetention, HistoryState, ProviderId, Settings,
} from "./types";

interface Reading extends HistoryPoint {
  provider: ProviderId;
  account: string;
  key: HistoryLimitKey;
  label: string;
}

const currentClaude = "a".repeat(64);
const currentCodex = "b".repeat(64);
const otherClaude = "c".repeat(64);

function dateLabel(date: Date): string {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

function localDay(date: Date, offset = 0): Date {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate() + offset);
}

function seconds(date: Date): number { return Math.floor(date.getTime() / 1000); }

function sameWindow(left: HistoryLimitKey, right: HistoryLimitKey): boolean {
  return left.id === right.id && left.window_seconds === right.window_seconds && left.provenance === right.provenance;
}

function point(reading: HistoryPoint): HistoryPoint {
  return { observed_at: reading.observed_at, used_fraction: reading.used_fraction, resets_at: reading.resets_at, break_before: [...reading.break_before] };
}

function highest(readings: HistoryPoint[]): HistoryPoint | null {
  return readings.reduce<HistoryPoint | null>((peak, reading) => peak === null || reading.used_fraction > peak.used_fraction ? point(reading) : peak, null);
}

function sampleReadings(now: number, mode: string): Reading[] {
  const today = new Date(now * 1000);
  const readings: Reading[] = [];
  for (let age = 34; age >= 0; age -= 1) {
    if (age === 3 || age === 11 || (mode === "sparse" && age !== 1 && age !== 8 && age !== 22)) continue;
    const day = localDay(today, -age);
    for (let slot = 0; slot < 38; slot += 1) {
      if (slot >= 14 && slot <= 19) continue;
      const at = new Date(day.getFullYear(), day.getMonth(), day.getDate(), 8, 30 + slot * 15);
      const observed = seconds(at);
      if (observed > now) continue;
      for (const provider of ["claude", "codex"] as const) {
        const account = provider === "codex" ? currentCodex : mode === "accounts" && age <= 5 ? otherClaude : currentClaude;
        const weekly = ((34 - age) % 7) / 8 + slot / 400;
        const session = (slot % 20) / 24 + ((34 - age) % 4) / 30;
        const windows = provider === "claude"
          ? [{ id: "session", label: "5-hour", duration: 18000, used: session }, { id: "weekly", label: "Weekly", duration: 604800, used: weekly }]
          : [{ id: "rate_limit:primary_window", label: "Weekly", duration: 604800, used: weekly + 0.08 }];
        for (const window of windows) {
          const breaks: HistoryBreak[] = [];
          if (slot === 20) breaks.push("missing_time");
          if (window.duration === 18000 && slot === 20) breaks.push("reset_changed", "reset_boundary", "usage_decreased");
          readings.push({
            provider, account, label: window.label,
            key: { id: window.id, window_seconds: window.duration, provenance: "official" },
            observed_at: observed, used_fraction: Math.min(0.98, window.used),
            resets_at: window.duration === 18000
              ? seconds(new Date(day.getFullYear(), day.getMonth(), day.getDate(), slot < 20 ? 13 : 18, 30))
              : seconds(localDay(day, 7 - ((34 - age) % 7))),
            break_before: breaks,
          });
        }
      }
    }
  }
  return readings;
}

export function createHistoryPreview(search: string) {
  const parameters = new URLSearchParams(search);
  const mode = parameters.get("history") ?? "empty";
  const now = Math.floor(Date.now() / 1000);
  let recording = ["ready", "waiting", "sparse", "error", "accounts"].includes(mode);
  let retention: HistoryRetention = "forever";
  let error = mode === "error" ? "A saved history file could not be read. Your usage readings are still available in the overview." : null;
  let readings = ["ready", "paused", "sparse", "accounts", "error"].includes(mode) ? sampleReadings(now, mode) : [];

  function catalog(): HistoryCatalog {
    if (error) throw new Error(error);
    const accounts: HistoryAccount[] = [];
    for (const reading of readings) {
      let account = accounts.find((candidate) => candidate.provider === reading.provider && candidate.account_key === reading.account);
      if (!account) {
        account = { provider: reading.provider, account_key: reading.account, first_recorded_at: reading.observed_at, last_recorded_at: reading.observed_at, windows: [] };
        accounts.push(account);
      }
      account.last_recorded_at = reading.observed_at;
      let window = account.windows.find((candidate) => sameWindow(candidate.key, reading.key));
      if (!window) {
        window = { key: { ...reading.key }, label: reading.label, first_recorded_at: reading.observed_at, last_recorded_at: reading.observed_at };
        account.windows.push(window);
      }
      window.last_recorded_at = reading.observed_at;
    }
    return {
      accounts: accounts.sort((left, right) => right.last_recorded_at - left.last_recorded_at),
      current_accounts: (["claude", "codex"] as const).map((provider) => {
        const verified = recording && mode !== "waiting" && parameters.get("disconnected") !== provider && parameters.get("disconnected") !== "both";
        return {
          provider,
          account_key: !verified ? null : provider === "codex" ? currentCodex : mode === "accounts" ? otherClaude : currentClaude,
          verified_at: verified ? now : null,
        };
      }),
    };
  }

  function query(request: HistoryRequest, threshold: number, originalReadings = false): HistoryQuery {
    if (error) throw new Error(error);
    const selected = readings.filter((reading) => reading.provider === request.provider && reading.account === request.account_key && sameWindow(reading.key, request.limit));
    const today = localDay(new Date(now * 1000));
    const kind = request.range.kind;
    let first = today;
    let until = now + 1;
    if (kind === "day" && "date" in request.range) {
      const parts = request.range.date.split("-").map(Number);
      const [year, month, day] = parts;
      if (year === undefined || month === undefined || day === undefined) throw new Error("Choose a valid history date.");
      first = new Date(year, month - 1, day);
      if (dateLabel(first) !== request.range.date || first > today) throw new Error("Choose a valid history date.");
      until = Math.min(until, seconds(localDay(first, 1)));
    } else if (kind === "days7" || kind === "days30") first = localDay(today, kind === "days7" ? -6 : -29);
    else if (kind === "all_time" && selected[0]) first = localDay(new Date(selected[0].observed_at * 1000));
    const from = seconds(first);
    const matches = selected.filter((reading) => reading.observed_at >= from && reading.observed_at < until);
    if (originalReadings && matches.length > 100_000) throw new Error("This history export is too large. Choose a shorter period.");
    const days: HistoryDay[] = [];
    if (kind !== "all_time" || matches.length > 0) {
      for (let day = first; seconds(day) < until; day = localDay(day, 1)) {
        const starts_at = seconds(day);
        const ends_at = seconds(localDay(day, 1));
        const samples = matches.filter((reading) => reading.observed_at >= starts_at && reading.observed_at < ends_at);
        days.push({ date: dateLabel(day), starts_at, ends_at, sample_count: samples.length, peak: highest(samples) });
      }
    }
    return {
      request, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone, from, until, days,
      points: originalReadings || kind === "today" || kind === "day" ? matches.map(point) : [],
      observations: {
        peak: highest(matches), days_with_readings: days.filter((day) => day.sample_count > 0).length,
        days_below_threshold: days.filter((day) => day.peak !== null && day.peak.used_fraction > (100 - threshold) / 100).length,
        days_in_range: days.length, threshold_remaining: threshold,
      },
    };
  }

  function csv(requests: HistoryRequest[], threshold: number): Uint8Array {
    if (requests.length === 0 || requests.length > 2 || new Set(requests.map((request) => request.provider)).size !== requests.length
      || requests.some((request) => JSON.stringify(request.range) !== JSON.stringify(requests[0]?.range))) {
      throw new Error("Choose one or two providers with the same history period.");
    }
    const encoder = new TextEncoder();
    const rows = ["observed_at_utc,timezone,local_date,provider,account_key,limit_id,window_seconds,used_fraction,remaining_fraction,resets_at_utc,provenance,break_before\r\n"];
    let bytes = encoder.encode(rows[0]).length;
    const utc = (at: number) => new Date(at * 1000).toISOString().replace(".000Z", "Z");
    const quote = (value: string) => `"${/^[=+\-@]/.test(value.trimStart()) || /^[\t\r\n]/.test(value) ? "'" : ""}${value.replaceAll('"', '""')}"`;
    for (const request of requests) {
      const result = query(request, threshold, true);
      for (const reading of result.points) {
        const day = result.days.find((day) => day.starts_at <= reading.observed_at && day.ends_at > reading.observed_at);
        if (!day) throw new Error("A saved usage reading is invalid.");
        const row = [
          utc(reading.observed_at), result.timezone, day.date, request.provider, request.account_key,
          request.limit.id, request.limit.window_seconds === null ? "" : String(request.limit.window_seconds),
          String(reading.used_fraction), String(1 - reading.used_fraction), reading.resets_at === null ? "" : utc(reading.resets_at),
          request.limit.provenance, reading.break_before.join(";"),
        ].map(quote).join(",") + "\r\n";
        bytes += encoder.encode(row).length;
        if (bytes > 32 * 1024 * 1024) throw new Error("This history export is too large. Choose a shorter period.");
        rows.push(row);
      }
    }
    return encoder.encode(rows.join(""));
  }

  function state(_settings: Settings): HistoryState {
    const records = new Set(readings.map((reading) => `${reading.account}:${reading.observed_at}`)).size;
    return {
      recording, retention, error,
      info: { records, bytes: records * 540, last_recorded_at: readings.at(-1)?.observed_at ?? null },
      provider_issues: mode === "waiting" && recording ? [{ provider: "claude", message: "Waiting for account details before saving readings." }] : [],
    };
  }

  return {
    catalog, query, csv, state,
    setRecording(enabled: boolean) { recording = enabled; },
    setRetention(value: HistoryRetention) {
      retention = value;
      if (value !== "forever") readings = readings.filter((reading) => reading.observed_at >= now - (value === "days30" ? 30 : 90) * 86400);
    },
    clear() { readings = []; error = null; },
  };
}

let shared: ReturnType<typeof createHistoryPreview> | undefined;
export function getHistoryPreview(search = window.location.search) {
  shared ??= createHistoryPreview(search);
  return shared;
}
