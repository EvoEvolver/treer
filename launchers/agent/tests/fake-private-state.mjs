import { createServer } from "node:http";
import { randomBytes } from "node:crypto";

/** Sealed Controller quota fixture: synthetic identity only, no Treer services. */
export async function fakePrivateState(agent, credential = "fixture-only", options = {}) {
  const events = [], reservations = new Set();
  const server = createServer(async (request, response) => {
    const reply = (status, body) => { response.writeHead(status, { "content-type": "application/json" }); response.end(JSON.stringify(body)); };
    if (request.headers["x-treer-agent-id"] !== agent || request.headers["x-treer-workload-credential"] !== credential) return reply(401, { error: { code: "workload_identity_required" } });
    const path = request.url;
    events.push({ method: request.method, path });
    if (path === "/api/private-state") return reply(200, { feature: "agent.private-state.v1", status: options.status ?? "available" });
    if (path === "/api/private-state/reservations") {
      if (options.reserveError) return reply(413, { error: { code: options.reserveError } });
      const id = `psr_${randomBytes(32).toString("hex")}`;
      reservations.add(id); return reply(201, { reservation_id: id });
    }
    const id = path.split("/")[4];
    if (request.method === "DELETE") return reply(200, { released: reservations.delete(id) });
    if (path.endsWith("/commit")) {
      options.onCommit?.();
      if (options.hangCommit) return;
      if (options.commitError) return reply(options.commitError === "private_state_reservation_expired" ? 409 : 413, { error: { code: options.commitError } });
      const committed = reservations.delete(id);
      return reply(committed ? 200 : 409, committed ? { usage: {} } : { error: { code: "private_state_reservation_expired" } });
    }
    reply(404, { error: { code: "not_found" } });
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  return { url: `http://127.0.0.1:${server.address().port}`, events, reservations, options, close: () => new Promise(resolve => server.close(resolve)) };
}
