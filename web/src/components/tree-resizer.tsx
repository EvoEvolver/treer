import { useEffect, useRef, useState, type CSSProperties, type PointerEvent, type KeyboardEvent } from "react"

import { ChevronLeft, ChevronRight } from "lucide-react"

const DEFAULT_WIDTH = 272
const MIN_WIDTH = 240
const MAX_WIDTH = 480
const clamp = (value: number, max: number) => Math.round(Math.max(MIN_WIDTH, Math.min(max, value)))

export function useTreeWidth(userId: string | undefined, onFinish: () => void) {
  const [preference, setPreference] = useState(DEFAULT_WIDTH)
  const [collapsed, setCollapsed] = useState(false)
  const [viewport, setViewport] = useState(() => window.innerWidth)
  const [dragging, setDragging] = useState(false)
  const drag = useRef<{ id: number; x: number; width: number } | null>(null)
  const finishRef = useRef(onFinish)
  finishRef.current = onFinish
  const key = userId ? `treer:tree-width:v1:${userId}` : null
  const collapsedKey = userId ? `treer:tree-collapsed:v1:${userId}` : null
  const max = Math.max(MIN_WIDTH, Math.min(MAX_WIDTH, Math.floor(viewport * 0.45), viewport - 408))
  const width = clamp(preference, max)

  useEffect(() => {
    let value = DEFAULT_WIDTH
    let hidden = false
    try {
      const stored = key ? window.localStorage.getItem(key) : null
      if (stored && Number.isFinite(Number(stored))) value = clamp(Number(stored), MAX_WIDTH)
      hidden = collapsedKey ? window.localStorage.getItem(collapsedKey) === "true" : false
    } catch { /* Browser storage is optional. */ }
    setPreference(value)
    setCollapsed(hidden)
  }, [key, collapsedKey])

  function finish() {
    if (!drag.current) return
    drag.current = null
    setDragging(false)
    finishRef.current()
  }
  useEffect(() => {
    const resize = () => { setViewport(window.innerWidth); if (window.innerWidth < 768) finish() }
    window.addEventListener("resize", resize)
    window.addEventListener("blur", finish)
    return () => { window.removeEventListener("resize", resize); window.removeEventListener("blur", finish) }
  }, [])

  function update(value: number) {
    const next = clamp(value, max)
    setPreference(next)
    try { if (key) window.localStorage.setItem(key, String(next)) } catch { /* Keep in-memory resizing. */ }
  }
  return {
    width, max, dragging, collapsed,
    toggle() {
      finish()
      const next = !collapsed
      setCollapsed(next)
      try { if (collapsedKey) window.localStorage.setItem(collapsedKey, String(next)) } catch { /* Keep in-memory toggling. */ }
      finishRef.current()
    },
    style: { "--tree-width": collapsed ? "0px" : `${width}px`, "--tree-divider": collapsed ? "24px" : "8px" } as CSSProperties,
    separator: {
      role: "separator", tabIndex: 0, "aria-label": "Resize tree sidebar", "aria-orientation": "vertical" as const,
      "aria-controls": "tree-sidebar", "aria-valuemin": MIN_WIDTH, "aria-valuemax": max, "aria-valuenow": width,
      "aria-valuetext": `${width} pixels`,
      onPointerDown(event: PointerEvent<HTMLDivElement>) {
        if (viewport < 768 || !event.isPrimary || event.button !== 0) return
        event.preventDefault()
        event.currentTarget.focus()
        event.currentTarget.setPointerCapture(event.pointerId)
        drag.current = { id: event.pointerId, x: event.clientX, width }
        setDragging(true)
      },
      onPointerMove(event: PointerEvent<HTMLDivElement>) {
        if (drag.current?.id === event.pointerId) update(drag.current.width + event.clientX - drag.current.x)
      },
      onPointerUp(event: PointerEvent<HTMLDivElement>) { if (drag.current?.id === event.pointerId) finish() },
      onPointerCancel(event: PointerEvent<HTMLDivElement>) { if (drag.current?.id === event.pointerId) finish() },
      onLostPointerCapture(event: PointerEvent<HTMLDivElement>) { if (drag.current?.id === event.pointerId) finish() },
      onDoubleClick() { update(DEFAULT_WIDTH); finishRef.current() },
      onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
        const step = event.shiftKey ? 64 : 16
        const value = { ArrowLeft: width - step, ArrowRight: width + step, Home: MIN_WIDTH, End: max }[event.key]
        if (value === undefined) return
        event.preventDefault(); update(value); finishRef.current()
      },
    },
  }
}

export function TreeResizer({ settings }: { settings: ReturnType<typeof useTreeWidth> }) {
  return <div className="relative hidden min-h-0 bg-sidebar md:block">
    {!settings.collapsed && <div {...settings.separator} className="absolute inset-y-0 -left-2 z-20 w-6 touch-none cursor-col-resize outline-none before:absolute before:inset-y-0 before:left-2 before:w-2 before:border-x before:border-border hover:before:bg-accent focus-visible:before:bg-ring/30" />}
    <button type="button" aria-label={settings.collapsed ? "Expand tree sidebar" : "Collapse tree sidebar"} aria-expanded={!settings.collapsed} aria-controls="tree-sidebar" title={settings.collapsed ? "Expand tree sidebar" : "Collapse tree sidebar"} onClick={settings.toggle} className={`group absolute top-1/2 z-30 grid size-11 -translate-y-1/2 place-items-center rounded outline-none ${settings.collapsed ? "left-0" : "left-1/2 -translate-x-1/2"}`}>
      <span aria-hidden="true" className={`pointer-events-none flex h-7 w-4 items-center justify-center rounded-sm border bg-sidebar text-muted-foreground shadow-sm group-hover:bg-accent group-hover:text-foreground group-focus-visible:ring-2 group-focus-visible:ring-ring ${settings.collapsed ? "-translate-x-2.5" : ""}`}>
        {settings.collapsed ? <ChevronRight className="size-3" /> : <ChevronLeft className="size-3" />}
      </span>
    </button>
    {settings.dragging && <div aria-hidden="true" className="fixed inset-0 z-[200] cursor-col-resize touch-none" />}
  </div>
}
