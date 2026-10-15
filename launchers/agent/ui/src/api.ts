// The only network helper. Paths are always relative so the page works behind Treer's iframe tunnel (<base href="./">).
export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, { ...init, headers: { "content-type": "application/json", ...(init?.headers ?? {}) } });
  const payload = response.status === 204 ? null : await response.json();
  if (!response.ok) {
    const error = (payload as { error?: { message?: string } | string } | null)?.error;
    throw new Error((typeof error === "string" ? error : error?.message) || `HTTP ${response.status}`);
  }
  return payload as T;
}

export function socketUrl(): URL {
  const url = new URL("ws", document.baseURI);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url;
}
