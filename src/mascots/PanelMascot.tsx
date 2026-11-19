import { useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import type { CSSProperties } from "react";
import type { ProviderId, ProviderState, Settings } from "../types";
import { confirmedRefill, mascotReading, refillBaseline } from "./mascot-reading";
import type { MascotReading } from "./mascot-reading";
import "./PanelMascot.css";
import "./clawd-motion.css";
import "./codex-motion.css";

interface PanelMascotProps {
  provider: ProviderId;
  pose: MascotReading["pose"];
  remaining: number | null;
  refillKey: number;
  still: boolean;
}

type Motion = { name: "patrol"; duration: number } | { name: "idle" | "refill" | "glance" };

const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

function motionAllowed() {
  return !document.hidden && !reducedMotion.matches;
}

function subscribeToMotion(change: () => void) {
  reducedMotion.addEventListener("change", change);
  document.addEventListener("visibilitychange", change);
  return () => {
    reducedMotion.removeEventListener("change", change);
    document.removeEventListener("visibilitychange", change);
  };
}

export function ProviderMascot({ provider, settings, now, expired, recovering, hasPercentage }: {
  provider: ProviderState;
  settings: Settings;
  now: number;
  expired: boolean;
  recovering: boolean;
  hasPercentage: boolean;
}) {
  // Keep the accepted observation across the clock tick between a reset and the next check.
  const observedAt = Math.min(now, provider.snapshot?.fetched_at ?? NaN);
  const reading = useMemo(() => mascotReading(provider, settings, observedAt), [provider, settings, observedAt]);
  const previous = useRef<MascotReading | null>(null);
  const [refillKey, setRefillKey] = useState(0);

  useLayoutEffect(() => {
    if (!recovering && confirmedRefill(previous.current, reading)) setRefillKey((key) => key + 1);
    previous.current = recovering ? null : refillBaseline(previous.current, reading, provider);
  }, [reading, recovering, provider]);

  if (!hasPercentage) return null;
  return <PanelMascot provider={provider.id} pose={expired || recovering ? "unavailable" : reading.pose}
    remaining={reading.remaining} refillKey={refillKey} still={!settings.mascot_animation} />;
}

function PanelMascot({ provider, pose, remaining, refillKey, still }: PanelMascotProps) {
  const visibleMotion = useSyncExternalStore(subscribeToMotion, motionAllowed);
  const canMove = visibleMotion && !still && (pose === "normal" || pose === "low");
  const previous = useRef({ provider, pose, refillKey });
  const hasMoved = useRef(false);
  const trips = useRef(0);
  const [motion, setMotion] = useState<Motion | null>(null);

  useLayoutEffect(() => {
    const before = previous.current;
    previous.current = { provider, pose, refillKey };
    if (!canMove || provider !== before.provider) {
      hasMoved.current = false;
      trips.current = 0;
      setMotion(null);
    } else if (refillKey > 0 && refillKey !== before.refillKey) {
      setMotion({ name: "refill" });
    } else if (pose !== before.pose || refillKey !== before.refillKey) {
      setMotion(pose === "low" && before.pose !== "low" ? { name: "glance" } : null);
    }
  }, [provider, pose, refillKey, canMove]);

  useLayoutEffect(() => {
    if (!canMove || remaining === null || remaining <= 0) return;
    if (motion !== null) {
      hasMoved.current = true;
      return;
    }
    const spent = 100 - remaining;
    const rest = 430 + spent * 8 + (pose === "low" ? 270 : 0);
    const delay = (hasMoved.current ? rest : 100) + (provider === "codex" ? 250 : 0);
    const timer = window.setTimeout(() => {
      if (!motionAllowed()) return;
      // A blink between trips breaks up the walk without waking a resting mascot.
      if (trips.current === 2) {
        trips.current = 0;
        setMotion({ name: "idle" });
      } else {
        trips.current++;
        // Finish at the speed of the starting reading if a new percentage arrives mid-step.
        const duration = (provider === "claude" ? 3400 : 3900) + spent * 8 + (pose === "low" ? 450 : 0);
        setMotion({ name: "patrol", duration });
      }
    }, delay);
    return () => window.clearTimeout(timer);
  }, [canMove, remaining, motion, pose, provider]);

  return (
    <span className="panel-mascot" aria-hidden="true" data-provider={provider}
      data-pose={pose} data-motion={canMove ? motion?.name : undefined}
      style={motion?.name === "patrol" ? { "--patrol-duration": `${motion.duration}ms` } as CSSProperties : undefined}>
      <span key={`${refillKey}-${motion?.name ?? "rest"}`} className="panel-mascot-art"
        onAnimationEnd={(event) => {
          // The feet and face can finish their own frames before the trip ends.
          if (event.target === event.currentTarget && !event.nativeEvent.pseudoElement) setMotion(null);
        }} />
    </span>
  );
}
