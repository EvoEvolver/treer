// The only place that talks to the network. Paths are resolved against the document base so the page
// keeps working below Treer's tunnel prefix (apps/GUIDELINES.md); never use origin-absolute URLs.
export class ApiError extends Error {
  constructor(message: string, readonly status: number, readonly code?: string) { super(message); }
}

export async function api<T>(path: string, method = "GET", body?: unknown): Promise<T> {
  const response = await fetch(new URL(path, document.baseURI), {
    method, credentials: "same-origin",
    headers: { Accept: "application/json", "Content-Type": "application/json", "X-Issues-Request": "1" },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  });
  const result = await response.json();
  if (!response.ok) throw new ApiError(result.error?.message || "Request failed", response.status, result.error?.code);
  return result as T;
}
