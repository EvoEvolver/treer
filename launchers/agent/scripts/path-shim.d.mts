export function shimText(node: string, entry: string, platform?: string): string;
export function shimPath(env?: Record<string, string | undefined>, platform?: string): string;
export function installShim(node: string, entry: string): { path: string; rollback(): void };
