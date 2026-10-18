import { securityHeaders } from './security-headers'

interface TreerAppEnv {
  ASSETS: Fetcher
  TREER_ENVIRONMENT: string
  TREER_PROXY_PUBLIC_URL: string
  VERSION_METADATA: WorkerVersionMetadata
}


export default {
  async fetch(request, env): Promise<Response> {
    const nonce = btoa(String.fromCharCode(...Array.from(crypto.getRandomValues(new Uint8Array(24)))))
    let headers: Headers
    try { headers = securityHeaders(env.TREER_PROXY_PUBLIC_URL, nonce) }
    catch { return new Response('Invalid control-plane configuration', { status: 503, headers: { 'cache-control': 'no-store' } }) }
    const { pathname } = new URL(request.url)
    let response: Response
    if (pathname === '/health') {
      response = Response.json({ service: 'treer-app', status: 'ok', environment: env.TREER_ENVIRONMENT, version: env.VERSION_METADATA.id })
    } else if (pathname === '/config.json') {
      response = Response.json({ proxy_url: env.TREER_PROXY_PUBLIC_URL })
    } else {
      // HTML is nonce-bearing and must not reuse a browser's conditional response.
      const assetRequest = new Request(request)
      assetRequest.headers.delete('if-none-match')
      assetRequest.headers.delete('if-modified-since')
      response = await env.ASSETS.fetch(assetRequest)
    }
    const resultHeaders = new Headers(response.headers)
    headers.forEach((value, key) => resultHeaders.set(key, value))
    const html = (resultHeaders.get('content-type') ?? '').includes('text/html')
    // Console documents only: never apply this to API/config/asset responses.
    if (html) resultHeaders.set('cross-origin-opener-policy', 'same-origin')
    if (html || pathname === '/config.json' || pathname === '/health') {
      resultHeaders.set('cache-control', 'no-store')
      resultHeaders.delete('etag')
      resultHeaders.delete('last-modified')
    }
    const result = new Response(response.body, { status: response.status, statusText: response.statusText, headers: resultHeaders })
    if (!html) return result
    // The generated single-file bundle and early theme script both need a nonce.
    return new HTMLRewriter().on('script', { element(element) { element.setAttribute('nonce', nonce) } }).transform(result)
  },
} satisfies ExportedHandler<TreerAppEnv>
