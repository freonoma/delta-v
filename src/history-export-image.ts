import type { HistoryQuery, PercentageMode } from "./types";
import { chartMarkerIndices, chartPaths, chartPercent, chartPoint, chartTimeTicks, chartX, chartY, dailyChart, PLOT_BOTTOM, PLOT_HEIGHT, PLOT_LEFT, PLOT_RIGHT, PLOT_TOP } from "./history-chart";
import { wholePercent } from "./usage";

export interface HistoryExportSeries {
  label: string;
  accountLabel: string;
  result: HistoryQuery;
}

export interface HistoryImageOptions {
  series: HistoryExportSeries[];
  percentageMode: PercentageMode;
  theme: "light" | "dark";
  createdAt: number;
  sampleData?: boolean;
}

const names = { claude: "Claude", codex: "Codex" };
const palettes = {
  light: { background: "#f8f9fb", text: "#222b38", secondary: "#637083", line: "#dce2ea", claude: "#a96c50", codex: "#47796f" },
  dark: { background: "#1b2028", text: "#e9edf4", secondary: "#a7b0bf", line: "#333c49", claude: "#d3a086", codex: "#94b8af" },
};

export function exportDateLabel(result: HistoryQuery): string {
  const formatter = new Intl.DateTimeFormat(undefined, { timeZone: result.timezone, day: "numeric", month: "short", year: "numeric" });
  const start = result.days[0]?.starts_at ?? result.from;
  const end = result.days.at(-1)?.starts_at ?? result.until - 1;
  const first = formatter.format(start * 1000);
  const last = formatter.format(end * 1000);
  return first === last ? first : `${first} to ${last}`;
}

export function exportFilename(series: HistoryExportSeries[], format: "csv" | "png"): string {
  const result = series[0]?.result;
  const provider = series.length === 2 ? "both" : result?.request.provider ?? "usage";
  const range = result?.request.range;
  const period = range?.kind === "day" ? range.date : range?.kind.replace("days", "days-").replaceAll("_", "-") ?? "history";
  return `Delta-V-${provider}-${period}.${format}`;
}

export async function renderHistoryPng(options: HistoryImageOptions): Promise<Uint8Array> {
  const { series, percentageMode: mode } = options;
  if (series.length < 1 || series.length > 2) throw new Error("Choose one or two providers to export.");
  const width = 840;
  const sectionHeight = 365;
  const height = 122 + series.length * sectionHeight + 96;
  const canvas = document.createElement("canvas");
  canvas.width = width * 2;
  canvas.height = height * 2;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("Could not create the chart image.");
  const colors = palettes[options.theme];
  context.scale(2, 2);
  context.fillStyle = colors.background;
  context.fillRect(0, 0, width, height);

  function text(value: string, x: number, y: number, size = 12, bold = false, color = colors.text, maxWidth = width - x - 32) {
    context!.font = `${bold ? 600 : 400} ${size}px -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif`;
    context!.fillStyle = color;
    let fitted = value;
    if (context!.measureText(fitted).width > maxWidth) {
      while (fitted.length > 0 && context!.measureText(`${fitted}…`).width > maxWidth) fitted = fitted.slice(0, -1);
      fitted += "…";
    }
    context!.fillText(fitted, x, y);
  }
  function rule(y: number) {
    context!.strokeStyle = colors.line;
    context!.lineWidth = 1;
    context!.beginPath(); context!.moveTo(32, y); context!.lineTo(width - 32, y); context!.stroke();
  }

  text("Usage history", 32, 49, 26, true);
  const first = series[0];
  if (!first) throw new Error("There are no charts to export.");
  const rangeNames = { today: "Today", days7: "7 days", days30: "30 days", all_time: "All time", day: "Day" };
  text(`${rangeNames[first.result.request.range.kind]} · ${mode === "remaining" ? "Remaining allowance" : "Used allowance"}`, 32, 76, 13, false, colors.secondary);
  rule(99);

  series.forEach(({ label, accountLabel, result }, index) => {
    const top = 122 + index * sectionHeight;
    const color = colors[result.request.provider];
    context.fillStyle = color;
    context.beginPath(); context.arc(36, top + 1, 4, 0, Math.PI * 2); context.fill();
    text(`${names[result.request.provider]} · ${label}`, 49, top + 7, 18, true, colors.text, 590);
    const provenance = result.request.limit.provenance === "official" ? "Official readings"
      : result.request.limit.provenance === "local_estimate" ? "Local estimates" : "Unknown source";
    text(provenance, width - 165, top + 6, 11, false, colors.secondary);
    text(`${exportDateLabel(result)} · ${result.timezone} · ${accountLabel}`, 32, top + 32, 12, false, colors.secondary);
    const daily = dailyChart(result);
    const measure = daily ? mode === "remaining" ? "Lowest recorded remaining (%)" : "Highest recorded usage (%)"
      : mode === "remaining" ? "Remaining (%)" : "Used (%)";
    text(measure, 32, top + 58, 11, false, colors.secondary);
    const plotWidth = width - 64;
    const plotY = top + 69;
    const scaleY = 1.4;
    const paths = chartPaths(result, plotWidth, mode);
    context.save();
    context.translate(32, plotY);
    [100, 50, 0].forEach((percent) => {
      const y = (PLOT_TOP + (1 - percent / 100) * (PLOT_HEIGHT - PLOT_TOP - PLOT_BOTTOM)) * scaleY;
      context.strokeStyle = colors.line; context.lineWidth = 1;
      context.beginPath(); context.moveTo(PLOT_LEFT, y); context.lineTo(plotWidth - PLOT_RIGHT, y); context.stroke();
      text(String(percent), 0, y + 4, 10, false, colors.secondary, 29);
    });
    context.save(); context.scale(1, scaleY);
    context.strokeStyle = color; context.lineWidth = 1.5; context.lineJoin = "round";
    context.stroke(new Path2D(paths.line));
    context.strokeStyle = colors.secondary; context.lineWidth = 1; context.stroke(new Path2D(paths.empty));
    context.restore();
    context.fillStyle = color;
    for (const pointIndex of chartMarkerIndices(result, plotWidth)) {
      const point = chartPoint(result, pointIndex);
      if (!point) continue;
      context.beginPath();
      context.arc(chartX(result, pointIndex, plotWidth), chartY(point.used_fraction, mode) * scaleY, daily ? 2.6 : 1.8, 0, Math.PI * 2);
      context.fill();
    }
    const ticks = daily ? [...new Set([0, Math.floor((result.days.length - 1) / 2), result.days.length - 1])].flatMap((dayIndex) => {
      const day = result.days[dayIndex];
      return day ? [{ x: chartX(result, dayIndex, plotWidth), label: new Intl.DateTimeFormat(undefined, { timeZone: result.timezone, month: "short", day: "numeric" }).format(day.starts_at * 1000) }] : [];
    }) : chartTimeTicks(result, plotWidth);
    ticks.forEach((tick, tickIndex) => {
      context.textAlign = tickIndex === 0 ? "left" : tickIndex === ticks.length - 1 ? "right" : "center";
      text(tick.label, tick.x, (PLOT_HEIGHT - PLOT_BOTTOM) * scaleY + 23, 11, false, colors.secondary, 170);
    });
    context.textAlign = "left";
    if (!result.observations.peak) {
      context.textAlign = "center";
      text("No readings in this period", plotWidth / 2, chartY(0.5, mode) * scaleY - 10, 12, false, colors.secondary);
      context.textAlign = "left";
    }
    context.restore();
    const { peak, days_below_threshold: low, days_with_readings: covered, days_in_range: total, threshold_remaining: threshold } = result.observations;
    const value = peak ? `${wholePercent(chartPercent(peak.used_fraction, mode), mode)}%` : "No readings";
    const values = [value, `${low} ${low === 1 ? "day" : "days"}`, `${covered} / ${total}`];
    const labels = [mode === "remaining" ? "Lowest recorded remaining" : "Highest recorded usage", `Below ${threshold}% remaining`, "Days with readings"];
    values.forEach((value, column) => {
      const x = 32 + column * 265;
      text(value, x, top + 282, 22, true, colors.text, 250);
      text(labels[column] ?? "", x, top + 305, 11, false, colors.secondary, 250);
    });
    text(`${result.days.reduce((sum, day) => sum + day.sample_count, 0).toLocaleString()} saved readings`, 32, top + 331, 11, false, colors.secondary);
    rule(top + sectionHeight - 14);
  });

  const foot = 122 + series.length * sectionHeight;
  text(dailyChart(first.result) ? "Daily points show the closest recorded reading to the limit, not the amount used that day."
    : "Lines stop at gaps and window changes. A reset time alone does not create a reading.", 32, foot + 9, 11, false, colors.secondary);
  text("Blank periods have no saved readings. Observations do not measure tokens spent or productivity.", 32, foot + 28, 11, false, colors.secondary);
  text(`${options.sampleData ? "Sample data · " : ""}Exported ${new Date(options.createdAt * 1000).toLocaleString()}`, 32, foot + 65, 10, false, colors.secondary, 540);
  context.textAlign = "right";
  text("ΔV  Delta-V", width - 32, foot + 65, 15, true, colors.secondary, 200);
  context.textAlign = "left";
  const blob = await new Promise<Blob>((resolve, reject) => canvas.toBlob((value) => value ? resolve(value) : reject(new Error("Could not create the chart image.")), "image/png"));
  return new Uint8Array(await blob.arrayBuffer());
}
