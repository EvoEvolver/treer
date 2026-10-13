import type { IncomingHttpHeaders } from "node:http";
import { QueueError } from "./prompt-submission.js";

export function guardInterfaceRequest(headers: IncomingHttpHeaders, agentId: string, instanceId: string, port: number) {
  const source = headers["x-treer-interface-source"];
  const marked = source !== undefined || headers["x-treer-agent-id"] !== undefined || headers["x-treer-interface-instance"] !== undefined;
  const valid = Boolean(agentId) && (source === "proxy-ui" || source === "controller-api") && headers["x-treer-agent-id"] === agentId && headers["x-treer-interface-instance"] === instanceId;
  if (marked && !valid) throw new QueueError("Invalid AIS forwarding source, target or instance; update Proxy/Controller before this launcher", 403);
  const host = headers.host?.toLowerCase();
  const localHost = [`127.0.0.1:${port}`, `localhost:${port}`, `[::1]:${port}`].includes(host ?? "");
  if (!localHost && !(valid && source === "controller-api" && host === "agent-interface")) throw new QueueError("Invalid Agent Interface Host", 403);
  if (source === "controller-api" && headers.origin !== undefined) throw new QueueError("Controller AIS requests must not carry Origin", 403);
  const browser = headers.origin !== undefined || Object.keys(headers).some((name) => name.startsWith("sec-fetch-"));
  if (browser && !valid) throw new QueueError("Browser requests require registered Proxy UI forwarding", 403);
  return valid ? source : null;
}

/** Existing Controller-to-AIS instance headers, not browser-provided identity. */
export function verifyInterfaceCaller(headers: IncomingHttpHeaders, agentId: string, instanceId: string) {
  if (headers["x-treer-interface-source"] === "proxy-ui") return false;
  if (headers.origin !== undefined) throw new QueueError("Use the authenticated Controller for AIS queue operations", 403);
  if (!agentId) return false; // Standalone local clients have no attested sender.
  if (headers["x-treer-interface-source"] !== "controller-api" || headers["x-treer-agent-id"] !== agentId || headers["x-treer-interface-instance"] !== instanceId) throw new QueueError("AIS queue operations require controller-api source and registered target/instance; update the Controller", 403);
  return true;
}
