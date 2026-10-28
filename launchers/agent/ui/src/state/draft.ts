// Text other parts of the page ask the composer to insert (a file mention from the Files drawer).
// A tiny event channel, so the composer stays the only owner of its draft.
const listeners = new Set<(text: string) => void>();
export function insertIntoDraft(text: string) { listeners.forEach((listener) => listener(text)); }
export function onDraftInsert(listener: (text: string) => void): () => void { listeners.add(listener); return () => { listeners.delete(listener); }; }
