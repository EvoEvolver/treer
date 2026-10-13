export function securityHeaders(proxyUrl: string, nonce: string): Headers {
  const proxy = new URL(proxyUrl)
  if (!['http:', 'https:'].includes(proxy.protocol) || proxy.username || proxy.password) throw new Error('Invalid Proxy origin')
  const socket = new URL(proxy.origin)
  socket.protocol = proxy.protocol === 'https:' ? 'wss:' : 'ws:'
  return new Headers({
    'content-security-policy': [
      "default-src 'self'", `script-src 'nonce-${nonce}'`, "style-src 'self' 'unsafe-inline'",
      "img-src 'self' data: blob:", "font-src 'self' data:",
      `connect-src 'self' ${proxy.origin} ${socket.origin}`,
      `frame-src 'self' ${proxy.origin}`, "frame-ancestors 'none'", "object-src 'none'",
      "base-uri 'none'", "form-action 'self'",
    ].join('; '),
    'x-frame-options': 'DENY',
    'x-content-type-options': 'nosniff',
    'referrer-policy': 'no-referrer',
    // Do not disable microphone: the control plane may use Voice input.
  })
}

