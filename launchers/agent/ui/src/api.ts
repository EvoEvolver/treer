// The only network helper. Paths are always relative so the page works behind Treer's iframe tunnel (<base href="./">).
/** The only place the UI touches the network (enforced by check-ui-source): relative paths only, so the page works behind Treer's tunnel. */
export function fetchRaw(path: string, init?: RequestInit): Promise<Response> {
  if (/^(?:[a-z][a-z0-9+.-]*:|[\\/])/i.test(path)) throw new Error("Network paths must be relative");
  return fetch(path, init);
}

export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetchRaw(path, { ...init, headers: { "content-type": "application/json", ...(init?.headers ?? {}) } });
  const payload = response.status === 204 ? null : await response.json();
  if (!response.ok) {
    const error = (payload as { error?: { message?: string } | string } | null)?.error;
    throw new Error((typeof error === "string" ? error : error?.message) || `HTTP ${response.status}`);
  }
  return payload as T;
}

export function openSocket(): WebSocket {
  const url = new URL("ws", document.baseURI);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return new WebSocket(url);
}
