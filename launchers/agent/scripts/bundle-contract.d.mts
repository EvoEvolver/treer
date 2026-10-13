import type { BinaryLike } from "node:crypto";
export function digest(bytes: BinaryLike): string;
export function compatibility(contract: unknown, controller: unknown): { status: "unknown" | "compatible" | "incompatible"; reason: string | null };
