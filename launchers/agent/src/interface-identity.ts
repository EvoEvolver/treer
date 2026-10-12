import type { IncomingHttpHeaders } from "node:http";
import { QueueError } from "./prompt-submission.js";

/** Existing Controller-to-AIS instance headers, not browser-provided identity. */
export function verifyInterfaceCaller(headers: IncomingHttpHeaders, agentId: string, instanceId: string) {
  if (headers.origin !== undefined) throw new QueueError("Use the authenticated Controller for AIS queue operations", 403);
  if (!agentId) return false; // Standalone local clients have no attested sender.
  if (headers["x-treer-agent-id"] !== agentId || headers["x-treer-interface-instance"] !== instanceId) throw new QueueError("AIS queue operations require the registered Controller instance headers", 403);
  return true;
}
