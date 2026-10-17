/** Classify without exposing any supplier response, URL, credential or prompt. */
export function providerError(error: unknown): string {
  const text = typeof error === "string" ? error : "";
  if (/\b(?:401|403)\b|unauthori[sz]ed|forbidden|invalid[_ -](?:api[_ -]?)?key|authentication/i.test(text)) return "provider_auth_required: Provider authentication needs attention";
  if (/invalid[_ -](?:config|model)|model[_ -]not[_ -]found|configuration/i.test(text)) return "provider_configuration_error: Provider configuration needs attention";
  if (/\b429\b|rate[_ -]limit/i.test(text)) return "provider_rate_limited: Provider rate limit reached";
  if (/\b5\d\d\b|network|connection|timed?[_ -]?out|bad gateway|unavailable/i.test(text)) return "provider_unavailable: Provider temporarily unavailable";
  return "provider_error: Provider turn failed";
}
