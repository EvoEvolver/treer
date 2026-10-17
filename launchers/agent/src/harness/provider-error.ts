/** Classify without exposing any supplier response, URL, credential or prompt. */
export function providerError(error: unknown): string {
  const record = error && typeof error === "object" ? error as Record<string, unknown> : null;
  const text = typeof error === "string" ? error : typeof record?.message === "string" ? record.message : "";
  const codes = ["provider_auth_required: Provider authentication needs attention", "provider_configuration_error: Provider configuration needs attention", "provider_rate_limited: Provider rate limit reached", "provider_unavailable: Provider temporarily unavailable", "provider_error: Provider turn failed"];
  const classified = codes.find(value => value === text);
  if (classified) return classified;
  // Prefer an explicit HTTP status when the native error carries one; arbitrary
  // numbers in prose (token counts, identifiers, ports) are not status codes.
  const info = record?.codexErrorInfo;
  const detail = info && typeof info === "object" ? Object.values(info).find(value => value && typeof value === "object" && "httpStatusCode" in value) as Record<string, unknown> | undefined : undefined;
  const status = typeof record?.httpStatusCode === "number" ? record.httpStatusCode : detail?.httpStatusCode;
  if (status === 401 || status === 403 || /\bHTTP(?:\/\d(?:\.\d)?)?(?: error)?[\s:=]+(?:401|403)\b|\b(?:401 Unauthorized|403 Forbidden)\b|invalid[_ -](?:api[_ -]?)?key|authentication/i.test(text)) return codes[0];
  if (/invalid[_ -](?:config|model)|model[_ -]not[_ -]found|configuration/i.test(text)) return "provider_configuration_error: Provider configuration needs attention";
  if (status === 429 || /\bHTTP(?:\/\d(?:\.\d)?)?(?: error)?[\s:=]+429\b|429 Too Many Requests|rate[_ -]limit/i.test(text)) return codes[2];
  if (typeof status === "number" && status >= 500 && status <= 599 || /\bHTTP(?:\/\d(?:\.\d)?)?(?: error)?[\s:=]+5\d\d\b|\b(?:502 Bad Gateway|503 Service Unavailable|504 Gateway Timeout)\b|network error|connection (?:reset|refused|timed out)|\b(?:ECONNRESET|ECONNREFUSED|ETIMEDOUT)\b|request timed out/i.test(text)) return codes[3];
  return "provider_error: Provider turn failed";
}
