// xterm 5.5 scrolls only the normal buffer on touch, and ignores touch once an
// app enables mouse reporting (Codex, Claude Code) or in the alternate screen.
// Replay one-finger drags as pixel wheel events on the screen instead: xterm
// then scrolls the scrollback, sends wheel reports or arrow keys exactly as
// for a desktop wheel or trackpad, including a short momentum after a flick.
const DRAG_SLOP_PX = 6
const FRICTION = 0.95

export function bindTouchScroll(host: HTMLElement): () => void {
  let startY = 0
  let lastX = 0
  let lastY = 0
  let lastTime = 0
  let velocity = 0
  let dragging = false
  let momentum = 0

  const wheel = (deltaY: number) => {
    host.querySelector(".xterm-screen")?.dispatchEvent(new WheelEvent("wheel", {
      deltaY,
      deltaMode: WheelEvent.DOM_DELTA_PIXEL,
      clientX: lastX,
      clientY: lastY,
      bubbles: true,
      cancelable: true,
    }))
  }
  const stopMomentum = () => {
    cancelAnimationFrame(momentum)
    momentum = 0
  }

  const onStart = (event: TouchEvent) => {
    stopMomentum()
    dragging = false
    if (event.touches.length !== 1) return
    const touch = event.touches[0]
    startY = lastY = touch.clientY
    lastX = touch.clientX
    lastTime = event.timeStamp
    velocity = 0
    // xterm's own touch scrolling would move the viewport a second time.
    event.stopPropagation()
  }
  const onMove = (event: TouchEvent) => {
    if (event.touches.length !== 1) return
    const touch = event.touches[0]
    if (!dragging && Math.abs(touch.clientY - startY) < DRAG_SLOP_PX) return
    dragging = true
    event.preventDefault()
    event.stopPropagation()
    const deltaY = touch.clientY - lastY
    const elapsed = Math.max(1, event.timeStamp - lastTime)
    velocity = 0.8 * (deltaY / elapsed) + 0.2 * velocity
    lastX = touch.clientX
    lastY = touch.clientY
    lastTime = event.timeStamp
    // Content follows the finger: dragging down reveals older output.
    if (deltaY) wheel(-deltaY)
  }
  const onEnd = (event: TouchEvent) => {
    if (!dragging) return
    dragging = false
    event.stopPropagation()
    let perFrame = event.timeStamp - lastTime > 100 ? 0 : velocity * 16
    const step = () => {
      if (Math.abs(perFrame) < 0.5) {
        momentum = 0
        return
      }
      wheel(-perFrame)
      perFrame *= FRICTION
      momentum = requestAnimationFrame(step)
    }
    momentum = requestAnimationFrame(step)
  }

  const options = { capture: true, passive: false } as const
  host.addEventListener("touchstart", onStart, { capture: true, passive: true })
  host.addEventListener("touchmove", onMove, options)
  host.addEventListener("touchend", onEnd, options)
  host.addEventListener("touchcancel", onEnd, options)
  return () => {
    stopMomentum()
    host.removeEventListener("touchstart", onStart, options)
    host.removeEventListener("touchmove", onMove, options)
    host.removeEventListener("touchend", onEnd, options)
    host.removeEventListener("touchcancel", onEnd, options)
  }
}
