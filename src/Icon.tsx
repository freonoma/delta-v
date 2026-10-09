export function Icon({ name, spinning = false }: { name: "refresh" | "settings" | "quit" | "clock" | "close" | "chevron" | "external" | "pin" | "mini" | "expand" | "plus" | "minus" | "history" | "back"; spinning?: boolean }) {
  return (
    <svg className={spinning ? "icon spinning" : "icon"} viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {name === "refresh" && <><path d="M16.4 7A6.5 6.5 0 0 0 5 4.8L2.8 7M3.6 13A6.5 6.5 0 0 0 15 15.2l2.2-2.2" /><path d="M2.8 3.2V7h3.8m10.6 9.8V13h-3.8" /></>}
      {name === "settings" && <><path d="M4 3v14m6-14v14m6-14v14" /><path d="M2 7h4m2 6h4m2-8h4" strokeWidth="3" /></>}
      {name === "quit" && <><path d="M10 2.5v7M6 4.5a6.5 6.5 0 1 0 8 0" /></>}
      {name === "clock" && <><circle cx="10" cy="10" r="6.5" /><path d="M10 6v4l2.6 1.5" /></>}
      {name === "close" && <path d="m5 5 10 10M15 5 5 15" />}
      {name === "chevron" && <path d="m6 8 4 4 4-4" />}
      {name === "external" && <><path d="M11 3h6v6m0-6L8 12" /><path d="M8 4H4a1 1 0 0 0-1 1v11a1 1 0 0 0 1 1h11a1 1 0 0 0 1-1v-4" /></>}
      {name === "pin" && <><path d="M7 3h6m-5 0v5l-3 3v2h10v-2l-3-3V3M10 13v4" /></>}
      {name === "mini" && <path d="m3 3 5 5m-5 0h5V3m9 14-5-5m5 0h-5v5" />}
      {name === "expand" && <path d="m12 8 5-5m-5 0h5v5M8 12l-5 5m5 0H3v-5" />}
      {name === "plus" && <path d="M10 4v12M4 10h12" />}
      {name === "minus" && <path d="M4 10h12" />}
      {name === "history" && <path d="M3 3v14h14M6 12l3-5 4 3 4-6" />}
      {name === "back" && <path d="m8 4-6 6 6 6M2 10h16" />}
    </svg>
  );
}
