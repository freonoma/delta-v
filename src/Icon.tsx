export function Icon({ name, spinning = false }: { name: "refresh" | "settings" | "quit" | "clock" | "close" | "chevron" | "external" | "pin" | "mini" | "expand" | "plus" | "minus" | "history" | "back"; spinning?: boolean }) {
  return (
    <svg className={spinning ? "icon spinning" : "icon"} viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {name === "refresh" && <><path d="M16.3 11.7A6.5 6.5 0 1 1 15.6 6.8L17 8" /><path d="M17 4.2V8h-3.8" /></>}
      {name === "settings" && <><path d="M8.6 2.5h2.8l.4 1.8 1.2.5 1.6-.9 1.5 1.5-.9 1.6.5 1.2 1.8.4v2.8l-1.8.4-.5 1.2.9 1.6-1.5 1.5-1.6-.9-1.2.5-.4 1.8H8.6l-.4-1.8-1.2-.5-1.6.9-1.5-1.5.9-1.6-.5-1.2-1.8-.4V8.6l1.8-.4.5-1.2-.9-1.6 1.5-1.5 1.6.9 1.2-.5.4-1.8Z" /><circle cx="10" cy="10" r="2.5" /></>}
      {name === "quit" && <><path d="M10 2.5v6.7" /><path d="M5.8 4.8a6.5 6.5 0 1 0 8.4 0" /></>}
      {name === "clock" && <><circle cx="10" cy="10" r="6.5" /><path d="M10 6v4l2.6 1.5" /></>}
      {name === "close" && <path d="m5 5 10 10M15 5 5 15" />}
      {name === "chevron" && <path d="m6 8 4 4 4-4" />}
      {name === "external" && <><path d="M11 3.5h5.5V9m0-5.5L9 11" /><path d="M8 3.5H5A1.5 1.5 0 0 0 3.5 5v10A1.5 1.5 0 0 0 5 16.5h10a1.5 1.5 0 0 0 1.5-1.5v-3" /></>}
      {name === "pin" && <><path d="M7 3h6m-5 0v5l-3 3v2h10v-2l-3-3V3M10 13v4" /></>}
      {name === "mini" && <path d="m3 3 5 5m-5 0h5V3m9 14-5-5m5 0h-5v5" />}
      {name === "expand" && <path d="m12 8 5-5m-5 0h5v5M8 12l-5 5m5 0H3v-5" />}
      {name === "plus" && <path d="M10 4v12M4 10h12" />}
      {name === "minus" && <path d="M4 10h12" />}
      {name === "history" && <><path d="m3 8 1.4-1.2a6.5 6.5 0 1 1-.7 4.9" /><path d="M3 4.2V8h3.8M10 6.3v4l2.5 1.5" /></>}
      {name === "back" && <path d="m8.5 4.5-5.5 5.5 5.5 5.5M3 10h13.5" />}
    </svg>
  );
}
