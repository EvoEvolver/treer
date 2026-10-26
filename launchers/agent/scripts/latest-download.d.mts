export const MAX_BUNDLE_BYTES: number;
export const LATEST_URL: string;
export function releasePlatform(platform?: string, arch?: string): string;
export function releaseBase(value: string): URL;
export function releasePath(path: string): string;
export function objectUrl(value: string | URL): URL;
export function fetchObject(value: string | URL, limit: number, signal: AbortSignal): Promise<Buffer>;
export function fetchRelease(base: URL, path: string, limit: number, signal: AbortSignal): Promise<Buffer>;
export function validateFiles(manifest: unknown): void;
