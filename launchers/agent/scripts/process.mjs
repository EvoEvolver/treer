// Provider processes use argv vectors, never interpolate model/user text into a shell.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync, realpathSync, statSync } from "node:fs";
import { delimiter, dirname, extname, isAbsolute, join, relative, resolve, sep } from "node:path";

export function providerCommand(command, args = [], options = {}) {
  const platform = options.platform || process.platform, env = options.env || process.env;
  let file = command;
  if (platform === "win32" && !isAbsolute(file) && !/[\\/]/.test(file)) {
    const search = Object.entries(env).find(([name]) => name.toUpperCase() === "PATH")?.[1] || "";
    const suffixes = extname(file) ? [""] : [".exe", ".com", ".cmd", ".bat", ""];
    file = search.split(options.pathDelimiter || (process.platform === "win32" ? ";" : delimiter))
      .filter(directory => isAbsolute(directory)).flatMap(directory => suffixes.map(suffix => join(directory, command + suffix)))
      .find(candidate => existsSync(candidate) && statSync(candidate).isFile());
    if (!file) throw new Error(`Provider executable is not on PATH: ${command}; set its explicit executable path`);
  }
  if (platform === "win32" && /\.(cmd|bat)$/i.test(file)) {
    // Standard npm shims invoke one JS file beneath their own node_modules.
    // Do not execute arbitrary .cmd logic or invoke cmd.exe with untrusted argv.
    const shim = realpathSync(file), source = readFileSync(shim, "utf8");
    if (source.length > 65536) throw new Error("Provider npm shim is too large");
    const matches = [...source.matchAll(/"%(?:dp0%|~dp0)[\\/]((?:node_modules)[\\/][^"\r\n%]+\.(?:cjs|mjs|js))"/gi)];
    const entries = [...new Set(matches.map(match => resolve(dirname(shim), ...match[1].split(/[\\/]/))))];
    if (entries.length !== 1 || !statSync(entries[0], { throwIfNoEntry: false })?.isFile()) throw new Error("Unsupported provider .cmd/.bat shim; select its native .exe or JS entry explicitly");
    const entry = realpathSync(entries[0]), inside = relative(join(dirname(shim), "node_modules"), entry);
    if (isAbsolute(inside) || inside === ".." || inside.startsWith(`..${sep}`)) throw new Error("Provider npm shim entry escapes its node_modules; select a trusted JS entry explicitly");
    return { command: process.execPath, args: [entry, ...args] };
  }
  if (/\.(mjs|cjs|js)$/i.test(file)) return { command: process.execPath, args: [resolve(file), ...args] };
  return { command: file, args };
}

export function spawnProvider(command, args, options = {}) {
  const resolved = providerCommand(command, args, options);
  return spawn(resolved.command, resolved.args, { ...options, shell: false, detached: process.platform !== "win32", windowsHide: true });
}
export function runProviderSync(command, args, options = {}) {
  const resolved = providerCommand(command, args, options);
  return spawnSync(resolved.command, resolved.args, { ...options, shell: false, windowsHide: true });
}
export function stopProcessTree(child, signal = "SIGTERM") {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === "win32") {
    const systemRoot = process.env.SystemRoot || process.env.SYSTEMROOT || process.env.windir;
    if (!systemRoot || !isAbsolute(systemRoot)) throw new Error("SystemRoot is required to clean up Windows provider processes");
    const result = spawnSync(join(systemRoot, "System32/taskkill.exe"), ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore", windowsHide: true, timeout: 10000 });
    if (result.error || result.status !== 0) {
      // The OS may have reaped a process before Node delivers its exit event.
      try { process.kill(child.pid, 0); }
      catch (error) { if (error.code === "ESRCH") return; throw error; }
      throw new Error("Windows provider process-tree cleanup failed");
    }
  } else {
    try { process.kill(-child.pid, signal); }
    catch (error) { if (error.code !== "ESRCH") throw error; }
  }
}
