export function securityHeaders(proxyUrl: string, nonce: string, ingressUrl?: string): Headers {
  const proxy = new URL(proxyUrl)
  if (!['http:', 'https:'].includes(proxy.protocol) || proxy.username || proxy.password) throw new Error('Invalid Proxy origin')
  let frameIngress = ''
  if (ingressUrl) {
    const ingress = new URL(ingressUrl)
    const domain = ingress.hostname
    // Deployment configuration, never a read-model or Agent-supplied URL.
    if (ingress.protocol !== 'https:' || ingress.username || ingress.password || ingress.port || ingress.pathname !== '/' || ingress.search || ingress.hash ||
      !/^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]{0,62}$/.test(domain)) throw new Error('Invalid ingress origin')
    frameIngress = ` https://*.${domain}`
  }
  const socket = new URL(proxy.origin)
  socket.protocol = proxy.protocol === 'https:' ? 'wss:' : 'ws:'
  return new Headers({
    'content-security-policy': [
      "default-src 'self'", `script-src 'nonce-${nonce}'`, "style-src 'self' 'unsafe-inline'",
      "img-src 'self' data: blob:", "font-src 'self' data:",
      `connect-src 'self' ${proxy.origin} ${socket.origin}`,
      `frame-src 'self' ${proxy.origin}${frameIngress}`, "frame-ancestors 'none'", "object-src 'none'",
      "base-uri 'none'", "form-action 'self'",
    ].join('; '),
    'x-frame-options': 'DENY',
    'x-content-type-options': 'nosniff',
    'referrer-policy': 'no-referrer',
    // Do not disable microphone: the control plane may use Voice input.
  })
}

