// The only place that talks to the network. Paths are resolved against the document base so the page works below
// Treer's tunnel prefix; the custom header is the CSRF defence the server requires for every write.
export class ApiError extends Error {
  constructor(message: string, readonly status: number, readonly code?: string) { super(message); }
}

export async function api<T>(path: string, method = "GET", body?: unknown): Promise<T> {
  const response = await fetch(new URL(path, document.baseURI), {
    method, credentials: "same-origin",
    headers: { Accept: "application/json", "Content-Type": "application/json", "X-Telegram-Settings": "1" },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  });
  const result = await response.json().catch(() => ({}));
  if (!response.ok) throw new ApiError(result.error?.message || "Request failed", response.status, result.error?.code);
  return result as T;
}
