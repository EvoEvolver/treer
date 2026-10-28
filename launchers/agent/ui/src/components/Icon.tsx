// One inline sprite of 24px stroke icons; colour comes from currentColor.
const symbols: Record<string, string> = {
  back: "M19 12H5M11 6l-6 6 6 6",
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  file: "M6 3h8l4 4v14H6zM14 3v4h4",
  download: "M12 4v11M7 10.5l5 5 5-5M5 19.5h14",
  tasks: "M5 7h14M5 12h14M5 17h9",
  down: "M12 5v14M6 13l6 6 6-6",
  chev: "M9 6l6 6-6 6",
  close: "M6 6l12 12M18 6L6 18",
  check: "M5 12.5l4.5 4.5L19 7",
  term: "M4 7l5 5-5 5M12 18h8",
  spark: "M12 4l1.8 4.7 4.7 1.8-4.7 1.8L12 17l-1.8-4.7L5.5 10.5l4.7-1.8z",
  warn: "M12 4l9 16H3zM12 10v4M12 17v.5",
  queue: "M12 4l8 4-8 4-8-4zM4 12l8 4 8-4M4 16l8 4 8-4",
  plan: "M5 6h3M5 12h3M5 18h3M11 6h8M11 12h8M11 18h8",
  attach: "M20 11.5l-7.8 7.8a5 5 0 0 1-7.1-7.1l8.5-8.5a3.3 3.3 0 0 1 4.7 4.7l-8.5 8.5a1.7 1.7 0 0 1-2.4-2.4l7.8-7.8",
  tool: "M14.7 6.3a4 4 0 0 0-5 5L4 17l3 3 5.7-5.7a4 4 0 0 0 5-5l-2.6 2.6-2.4-.6-.6-2.4z",
  retry: "M4 12a8 8 0 0 1 14-5.3M20 12a8 8 0 0 1-14 5.3M18 3v4h-4M6 21v-4h4",
  copy: "M9 9h11v11H9zM5 15H4V4h11v1",
  open: "M14 4h6v6M20 4l-9 9M18 14v6H4V6h6",
  at: "M16 12a4 4 0 1 1-1.2-2.8M16 8v5a2.5 2.5 0 0 0 5 0v-1a9 9 0 1 0-3.5 7.1",
};

export function IconSprite() {
  return (
    <svg width="0" height="0" aria-hidden="true" focusable="false" className="sprite">
      {Object.entries(symbols).map(([name, d]) => <symbol key={name} id={`i-${name}`} viewBox="0 0 24 24"><path d={d} /></symbol>)}
      <symbol id="i-stop" viewBox="0 0 24 24"><path d="M7 7h10v10H7z" className="fill" /></symbol>
      <symbol id="i-now" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="3.5" className="fill" /></symbol>
      <symbol id="i-todo" viewBox="0 0 24 24"><circle cx="12" cy="12" r="7" /></symbol>
      <symbol id="i-search" viewBox="0 0 24 24"><circle cx="11" cy="11" r="6" /><path d="M16 16l4 4" /></symbol>
      <symbol id="i-goal" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="4" /><path d="M12 12h.01" /></symbol>
      <symbol id="i-gear" viewBox="0 0 24 24"><circle cx="12" cy="12" r="3" /><path d="M12 2.8v2.4M12 18.8v2.4M2.8 12h2.4M18.8 12h2.4M5.5 5.5l1.7 1.7M16.8 16.8l1.7 1.7M5.5 18.5l1.7-1.7M16.8 7.2l1.7-1.7" /><circle cx="12" cy="12" r="6.2" /></symbol>
      <symbol id="i-agent" viewBox="0 0 24 24"><rect x="5" y="8" width="14" height="10" rx="2" /><path d="M12 4v4M9 13h.01M15 13h.01" /></symbol>
    </svg>
  );
}

export function Icon({ name, className }: { name: string; className?: string }) {
  return <svg className={className ? `i ${className}` : "i"} aria-hidden="true" focusable="false"><use href={`#i-${name}`} /></svg>;
}
