import { request } from "node:http";
import { AttachmentError } from "./image-format.js";

export interface AttachmentAdmission {
  reserve(bytes: number): Promise<string>;
  commit(id: string): Promise<void>;
  release(id: string): Promise<void>;
}
export type PrivateStateStatus = "available" | "agent_private_state_unsafe" | "agent_private_state_inside_cwd" | "agent_private_state_unavailable" | "windows_requires_administrator";
const statuses = new Set<PrivateStateStatus>(["available", "agent_private_state_unsafe", "agent_private_state_inside_cwd", "agent_private_state_unavailable", "windows_requires_administrator"]);
const errors = new Set(["invalid_private_state_reservation", "private_state_agent_quota_exceeded", "private_state_global_quota_exceeded", "private_state_too_many_reservations", "private_state_disk_space_low", "private_state_reservation_expired", ...statuses]);

/** Workload credentials go only to the explicitly bound loopback Controller,
 * never into browser state, redirects, errors, or a provider request. */
export class PrivateStateClient implements AttachmentAdmission {
  private readonly base: URL;
  constructor(url: string, private readonly agent: string, private readonly credential: string, private readonly timeoutMs = 5000) {
    this.base = new URL(url);
    if (this.base.protocol !== "http:" || !["127.0.0.1", "[::1]"].includes(this.base.hostname) || this.base.username || this.base.password || this.base.pathname !== "/" || this.base.search || this.base.hash || !agent || !credential) throw new AttachmentError("agent_private_state_unavailable", 503);
  }
  private async call(path: string, method = "GET", body?: unknown): Promise<Record<string, unknown>> {
    try {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      return await new Promise((resolve, reject) => {
        const req = request(new URL(`/api/private-state${path}`, this.base), {
          method, signal: AbortSignal.timeout(this.timeoutMs), headers: {
            "x-treer-agent-id": this.agent, "x-treer-workload-credential": this.credential,
            ...(payload ? { "content-type": "application/json", "content-length": Buffer.byteLength(payload) } : {}),
          },
        }, response => {
          const chunks: Buffer[] = []; let size = 0;
          response.on("data", (chunk: Buffer) => { size += chunk.length; if (size > 65536) { response.destroy(); reject(new AttachmentError("agent_private_state_unavailable", 503)); } else chunks.push(chunk); });
          response.on("error", reject);
          response.on("end", () => {
            try {
              const data = JSON.parse(Buffer.concat(chunks).toString("utf8"));
              if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error();
              if ((response.statusCode ?? 500) >= 300) {
                const code = data.error?.code;
                throw new AttachmentError(typeof code === "string" && errors.has(code) ? code : "agent_private_state_unavailable", [400,409,413,429,507].includes(response.statusCode!) ? response.statusCode : 503);
              }
              resolve(data);
            } catch (error) { reject(error); }
          });
        });
        req.on("error", reject); req.end(payload);
      });
    } catch (error) {
      if (error instanceof AttachmentError) throw error;
      throw new AttachmentError("agent_private_state_unavailable", 503);
    }
  }
  async status(): Promise<PrivateStateStatus> {
    const data = await this.call("");
    if (data.feature !== "agent.private-state.v1" || !statuses.has(data.status as PrivateStateStatus)) throw new AttachmentError("agent_private_state_unavailable", 503);
    return data.status as PrivateStateStatus;
  }
  async reserve(bytes: number): Promise<string> {
    const data = await this.call("/reservations", "POST", { bytes });
    if (typeof data.reservation_id !== "string" || !/^psr_[a-f0-9]{64}$/.test(data.reservation_id)) throw new AttachmentError("agent_private_state_unavailable", 503);
    return data.reservation_id;
  }
  async commit(id: string) { await this.call(`/reservations/${id}/commit`, "POST"); }
  async release(id: string) { await this.call(`/reservations/${id}`, "DELETE"); }
}
