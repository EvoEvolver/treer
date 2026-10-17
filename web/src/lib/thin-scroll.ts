// Shows the overlay scrollbar of a ".thin-scroll" element only while it scrolls (wheel, touch, keyboard or drag).
// One capture listener covers every such element; scrolling itself is untouched, only a data attribute is toggled.
const HIDE_AFTER_MS = 800
const timers = new WeakMap<Element, number>()

export function installThinScroll(root: Document = document) {
  root.addEventListener("scroll", (event) => {
    const element = event.target
    if (!(element instanceof HTMLElement) || !element.classList.contains("thin-scroll")) return
    element.setAttribute("data-scrolling", "")
    window.clearTimeout(timers.get(element))
    timers.set(element, window.setTimeout(() => element.removeAttribute("data-scrolling"), HIDE_AFTER_MS))
  }, { capture: true, passive: true })
}
