/** Portable, secret-file-free snapshot consumed by Soul and other clients. */
export interface SessionFile {
  scope: "session" | "memory" | "project";
  /** Relative POSIX path within scope; no traversal, absolute paths or symlinks. */
  path: string;
  encoding: "base64";
  content: string;
  /** SHA-256 of decoded bytes, lowercase hexadecimal. */
  sha256: string;
}

export interface SessionBundle {
  schema: "treer.agent-session/v1";
  harness: string;
  sessionId: string;
  /** Original working directory, metadata only; never an import destination. */
  cwd: string;
  exportedAt: string;
  files: SessionFile[];
}

export interface SessionImportOptions {
  /** Dedicated new/empty directory; never merge into an existing project/home. */
  directory: string;
  /** Optional fresh UUID; defaults to a new UUID, never the source session ID. */
  sessionId?: string;
}

export interface ImportedSession {
  sessionId: string;
  /** Restored project directory, used as the new Agent's cwd. */
  cwd: string;
  /** Only provider storage selectors, never credentials or exported environment. */
  environment: Record<string, string>;
}

export class SessionTransferError extends Error {
  constructor(readonly code: "unsupported" | "not_ready" | "invalid_bundle" | "destination_exists" | "too_large", message: string) {
    super(message);
    this.name = "SessionTransferError";
  }
}
