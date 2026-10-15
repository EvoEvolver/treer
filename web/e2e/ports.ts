// Keep browser fixtures and Playwright-owned servers on the same port pair.
export function testPorts(value = process.env.PW_PORT ?? "4173") {
  const port = Number(value)
  if (!/^\d+$/.test(value) || !Number.isInteger(port) || port < 1 || port > 65534) {
    throw new Error("PW_PORT must be an integer from 1 to 65534; the security Worker uses PW_PORT + 1")
  }
  return { web: port, worker: port + 1 }
}

export const ports = testPorts()
export const workerOrigin = `http://127.0.0.1:${ports.worker}`
