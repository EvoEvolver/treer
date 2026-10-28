// Deciding whether the conversation keeps following the latest message.
//
// Only the reader opts out of following, by scrolling up themselves. Browsers also move a scroll container on their own:
// hiding a frame or tab (the Console hides inactive Agent tabs with display:none) lays the conversation out at zero size
// and resets it to the top, history replacing a placeholder clamps it, scroll anchoring shifts it while content above
// changes size. Treating those as "the reader scrolled up" left the view stuck at the top of a long conversation.

export const NEAR = 48; // px from the bottom that counts as "at the latest message"
export const INTENT_MS = 1000; // a scroll this soon after the reader's own wheel/touch/key/pointer input is theirs

export interface ScrollSample {
  top: number;
  height: number; // scrollHeight
  client: number; // clientHeight
}

export interface FollowState {
  pinned: boolean;
  /** The last position seen while visible, set by the reader or by following. */
  last: ScrollSample;
}

/**
 * What a scroll event means:
 * - `hidden`: the container has no size (a hidden frame/tab); ignore it and restore the view once it is shown again.
 * - `pin` / `unpin`: the reader reached the bottom, or moved up themselves.
 * - `follow`: the browser moved a following view away from the bottom (layout change or reset); go back to the bottom.
 * - `restore`: the browser reset a reader's position to the top (layout reset, not the reader); put them back.
 * - `keep`: nothing changes.
 */
export type ScrollDecision = "hidden" | "pin" | "unpin" | "follow" | "restore" | "keep";

export function decideScroll(state: FollowState, now: ScrollSample, readerActive: boolean): ScrollDecision {
  if (now.client === 0) return "hidden";
  const distance = now.height - now.top - now.client;
  if (distance < NEAR) return "pin";
  const movedUp = now.top < state.last.top - 2;
  if (!movedUp) return "keep";
  if (readerActive) return "unpin";
  // Not the reader. A jump to the very top, or a move that coincides with a size change, comes from layout.
  const reset = now.top <= 1 && state.last.top > NEAR;
  const resized = now.height !== state.last.height || now.client !== state.last.client;
  if (reset || resized) return state.pinned ? "follow" : reset ? "restore" : "keep";
  // Find-in-page, an in-page link or assistive technology moved the view: the reader went there.
  return "unpin";
}
