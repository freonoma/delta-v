import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, MouseEvent } from "react";
import type { HistoryQuery, PercentageMode } from "./types";
import {
  chartCount, chartIndexAt, chartKeyIndex, chartPaths, chartPercent,
  chartPoint, chartTimeTicks, chartX, chartY, dailyChart, PLOT_BOTTOM, PLOT_HEIGHT, PLOT_LEFT,
  PLOT_RIGHT, PLOT_TOP, timestampLabel,
} from "./history-chart";
import { wholePercent } from "./usage";
import "./HistoryChart.css";

interface Props {
  result: HistoryQuery;
  percentageMode: PercentageMode;
  onSelectDay: (date: string) => void;
}

export function HistoryChart({ result, percentageMode, onSelectDay }: Props) {
  const container = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(300);
  const [selected, setSelected] = useState<number | null>(null);
  const [hovered, setHovered] = useState<number | null>(null);
  const descriptionId = useId();
  const headingId = useId();
  const count = chartCount(result);
  const daily = dailyChart(result);
  const active = Math.max(-1, Math.min(count - 1, hovered ?? selected ?? count - 1));
  const point = chartPoint(result, active);
  const day = daily ? result.days[active] : result.days[0];
  const paths = useMemo(() => chartPaths(result, width, percentageMode), [result, width, percentageMode]);
  const measure = daily
    ? percentageMode === "remaining" ? "Lowest recorded remaining" : "Highest recorded usage"
    : percentageMode === "remaining" ? "Remaining" : "Used";
  const value = point ? `${wholePercent(chartPercent(point.used_fraction, percentageMode), percentageMode)}% ${percentageMode}` : "No readings";
  const date = day ? new Intl.DateTimeFormat(undefined, {
    timeZone: result.timezone, month: "short", day: "numeric", year: "numeric",
  }).format(day.starts_at * 1000) : "";
  const timestamp = point ? timestampLabel(point.observed_at, result.timezone) : "";
  const coverage = daily && day ? `${day.sample_count.toLocaleString()} ${day.sample_count === 1 ? "reading" : "readings"}` : "";
  const accessibleValue = daily
    ? `${date}. ${value}. ${coverage}.${point ? ` Recorded ${timestamp}.` : ""}`
    : `${value}.${point ? ` Recorded ${timestamp}.` : ""}`;
  const ticks = useMemo(() => {
    const dateFormat = new Intl.DateTimeFormat(undefined, { timeZone: result.timezone, month: "short", day: "numeric" });
    if (daily) {
      const indices = [...new Set([0, Math.floor((result.days.length - 1) / 2), result.days.length - 1])];
      return indices.flatMap((index) => {
        const currentDay = result.days[index];
        return currentDay ? [{ x: chartX(result, index, width), label: dateFormat.format(currentDay.starts_at * 1000) }] : [];
      });
    }
    return chartTimeTicks(result, width);
  }, [daily, result, width]);

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    const resize = new ResizeObserver(([entry]) => {
      if (entry) setWidth(Math.max(160, Math.min(1024, Math.round(entry.contentRect.width))));
    });
    resize.observe(element);
    return () => resize.disconnect();
  }, []);

  useEffect(() => { setSelected(null); setHovered(null); }, [result]);

  const pointerIndex = (event: MouseEvent<HTMLDivElement>) => {
    const bounds = event.currentTarget.getBoundingClientRect();
    const x = (event.clientX - bounds.left) / bounds.width * width;
    return chartIndexAt(result, (x - PLOT_LEFT) / (width - PLOT_LEFT - PLOT_RIGHT));
  };
  const selectDay = (index: number) => {
    const selectedDay = result.days[index];
    if (daily && selectedDay) onSelectDay(selectedDay.date);
  };
  const keyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const next = chartKeyIndex(active, event.key, count);
    if (next !== null) {
      event.preventDefault();
      setHovered(null);
      setSelected(next);
    } else if (event.key === "Enter" && daily) {
      event.preventDefault();
      selectDay(active);
    }
  };

  return <div className="history-chart" data-provider={result.request.provider} ref={container}>
    <p className="history-chart-measure" id={headingId}>{measure} <span>(%)</span></p>
    <div className="history-chart-plot" data-daily={daily} tabIndex={count > 0 ? 0 : undefined}
      role={count > 0 ? "slider" : "img"} aria-labelledby={headingId} aria-describedby={descriptionId}
      aria-valuemin={count > 0 ? 1 : undefined} aria-valuemax={count > 0 ? count : undefined}
      aria-valuenow={count > 0 ? active + 1 : undefined} aria-valuetext={count > 0 ? accessibleValue : undefined}
      onKeyDown={keyDown} onFocus={() => setHovered(null)} onPointerMove={(event) => setHovered(pointerIndex(event))}
      onPointerLeave={() => setHovered(null)} onClick={(event) => {
        if (event.button !== 0) return;
        const index = pointerIndex(event);
        setSelected(index);
        if (daily) selectDay(index);
      }}>
      <svg viewBox={`0 0 ${width} ${PLOT_HEIGHT}`} width="100%" height={PLOT_HEIGHT} aria-hidden="true">
        {[100, 50, 0].map((percent) => {
          const y = PLOT_TOP + (1 - percent / 100) * (PLOT_HEIGHT - PLOT_TOP - PLOT_BOTTOM);
          return <g key={percent}>
            <line className="history-chart-grid" x1={PLOT_LEFT} x2={width - PLOT_RIGHT} y1={y} y2={y} />
            <text className="history-chart-axis" x={PLOT_LEFT - 8} y={y + 3} textAnchor="end">{percent}</text>
          </g>;
        })}
        {ticks.map((tick, index) => <text className="history-chart-axis" key={`${tick.x}-${index}`}
          x={tick.x} y={PLOT_HEIGHT - 4} textAnchor={index === 0 ? "start" : index === ticks.length - 1 ? "end" : "middle"}>{tick.label}</text>)}
        <path className="history-chart-empty-ticks" d={paths.empty} />
        <path className="history-chart-line" d={paths.line} />
        <path className="history-chart-marks" d={paths.marks} />
        {active >= 0 && <line className="history-chart-guide" x1={chartX(result, active, width)} x2={chartX(result, active, width)} y1={PLOT_TOP} y2={PLOT_HEIGHT - PLOT_BOTTOM} />}
        {point && <circle className="history-chart-selected" cx={chartX(result, active, width)} cy={chartY(point.used_fraction, percentageMode)} r={3.5} />}
        {!point && result.points.length === 0 && !result.days.some((item) => item.peak) && <text className="history-chart-empty-label" x={(PLOT_LEFT + width - PLOT_RIGHT) / 2} y={PLOT_HEIGHT / 2} textAnchor="middle">No readings in this period</text>}
      </svg>
    </div>
    <div className="history-chart-inspector">
      <div className="history-chart-reading"><strong>{value}</strong><span>{daily ? `${date} · ${coverage}` : timestamp}</span>
      </div>
      <p className="history-chart-detail">{daily && point ? `Recorded ${timestamp}` : daily && day ? "Nothing was saved for this date." : point ? breakDescription(point.break_before) : "Only saved readings appear here."}</p>
    </div>
    <p className="history-chart-sr-only" id={descriptionId}>{daily
      ? "Daily points show the closest recorded reading to the limit, not the amount used that day. Use arrow keys to choose a date, Home or End to jump, and Enter to view its readings. Empty dates are available too."
      : "Use arrow keys to inspect each saved reading, or Home and End to jump. Lines stop at gaps and changes to the usage window. Unrecorded activity is not shown."}</p>
  </div>;
}

function breakDescription(reasons: HistoryQuery["points"][number]["break_before"]): string {
  if (reasons.includes("limit_unavailable")) return "This window was missing from the previous reading.";
  if (reasons.includes("missing_time")) return "There is a gap before this reading.";
  if (reasons.includes("reset_changed") || reasons.includes("reset_boundary")) return "The usage window changed before this reading.";
  if (reasons.includes("usage_decreased")) return "Usage decreased here. A reset was not confirmed.";
  return "Saved usage reading.";
}
