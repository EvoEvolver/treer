interface TreerAppEnv {
  ASSETS: Fetcher
  TREER_ENVIRONMENT: string
  TREER_PROXY_PUBLIC_URL: string
  VERSION_METADATA: WorkerVersionMetadata
}

function json(body: unknown) {
  return Response.json(body, {
    headers: { "cache-control": "no-store" },
  })
}

export default {
  fetch(request, env): Promise<Response> {
    const { pathname } = new URL(request.url)

    if (pathname === "/health") {
      return Promise.resolve(
        json({
          service: "treer-app",
          status: "ok",
          environment: env.TREER_ENVIRONMENT,
          version: env.VERSION_METADATA.id,
        }),
      )
    }

    if (pathname === "/config.json") {
      return Promise.resolve(json({ proxy_url: env.TREER_PROXY_PUBLIC_URL }))
    }

    return env.ASSETS.fetch(request)
  },
} satisfies ExportedHandler<TreerAppEnv>
