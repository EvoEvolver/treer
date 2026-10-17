// One inline sprite, drawn by the active look through stroke width, cap, join and size tokens.
const symbols: Record<string, string> = {
  folder: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z",
  file: "M6 3h8l4 4v14H6zM14 3v4h4",
  download: "M12 4v11M7 10.5l5 5 5-5M5 19.5h14",
  tasks: "M5 7h14M5 12h14M5 17h9",
  up: "M12 19V5M6 11l6-6 6 6",
  down: "M12 5v14M6 13l6 6 6-6",
  chev: "M9 6l6 6-6 6",
  close: "M6 6l12 12M18 6L6 18",
  check: "M5 12.5l4.5 4.5L19 7",
  term: "M4 7l5 5-5 5M12 18h8",
  edit: "M4 20l4-1L19 8l-3-3L5 16z",
  spark: "M12 4l1.8 4.7 4.7 1.8-4.7 1.8L12 17l-1.8-4.7L5.5 10.5l4.7-1.8z",
  warn: "M12 4l9 16H3zM12 10v4M12 17v.5",
  queue: "M12 4l8 4-8 4-8-4zM4 12l8 4 8-4M4 16l8 4 8-4",
  menu: "M4 7h16M4 12h16M4 17h16",
  plan: "M5 6h3M5 12h3M5 18h3M11 6h8M11 12h8M11 18h8",
};

export function IconSprite() {
  return (
    <svg width="0" height="0" aria-hidden="true" focusable="false" className="sprite">
      {Object.entries(symbols).map(([name, d]) => <symbol key={name} id={`i-${name}`} viewBox="0 0 24 24"><path d={d} /></symbol>)}
      <symbol id="i-theme" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" /><path d="M12 4a8 8 0 0 0 0 16z" className="fill" /></symbol>
      <symbol id="i-stop" viewBox="0 0 24 24"><path d="M7 7h10v10H7z" className="fill" /></symbol>
      <symbol id="i-now" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="3.5" className="fill" /></symbol>
      <symbol id="i-todo" viewBox="0 0 24 24"><circle cx="12" cy="12" r="7" /></symbol>
      <symbol id="i-search" viewBox="0 0 24 24"><circle cx="11" cy="11" r="6" /><path d="M16 16l4 4" /></symbol>
      <symbol id="i-goal" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" /><circle cx="12" cy="12" r="4" /><path d="M12 12h.01" /></symbol>
      <symbol id="i-look" viewBox="0 0 24 24"><circle cx="12" cy="12" r="8" /><path d="M8.5 11h.01M12 7.5h.01M15.5 11h.01" strokeWidth="2.2" /></symbol>
      <symbol id="i-agent" viewBox="0 0 24 24"><rect x="5" y="8" width="14" height="10" rx="2" /><path d="M12 4v4M9 13h.01M15 13h.01" /></symbol>
    </svg>
  );
}

export function Icon({ name, className }: { name: string; className?: string }) {
  return <svg className={className ? `i ${className}` : "i"} aria-hidden="true" focusable="false"><use href={`#i-${name}`} /></svg>;
}
