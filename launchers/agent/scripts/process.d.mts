import type { ChildProcess, ChildProcessWithoutNullStreams, SpawnOptions, SpawnSyncOptionsWithStringEncoding, SpawnSyncReturns } from "node:child_process";
export function providerCommand(command: string, args?: string[], options?: { platform?: string; env?: NodeJS.ProcessEnv; pathDelimiter?: string }): { command: string; args: string[] };
export function spawnProvider(command: string, args: string[], options?: SpawnOptions): ChildProcessWithoutNullStreams;
export function runProviderSync(command: string, args: string[], options: SpawnSyncOptionsWithStringEncoding): SpawnSyncReturns<string>;
export function stopProcessTree(child: ChildProcess | null | undefined, signal?: NodeJS.Signals): void;
