import { isDeepStrictEqual } from "node:util";

export const portableCommand = "treer-agent";
export function portableSpec(profile) { return { command: portableCommand, cwd: ".", args: profile.args }; }
export function managedProfile(current, profile, receipt) {
  if (!current) return true;
  const spec = { command: current.command, cwd: current.cwd, args: current.args ?? [] };
  if (isDeepStrictEqual(spec, portableSpec(profile))) return true;
  if (receipt && isDeepStrictEqual(spec, receipt)) return true;
  if (typeof current.command !== "string" || !Array.isArray(current.args)) return false;
  // Adopt only the known launcher command shapes with exactly this provider's
  // arguments. Names alone never authorize overwriting an owner's profile.
  const normalized = current.command.replaceAll("\\", "/");
  const legacy = /\/(?:agent-ui|agent)\/current\/launchers\/agent\/scripts\/treer-agent\.sh$/.test(normalized);
  const runner = current.args[0]?.replaceAll?.("\\", "/");
  const installed = /(?:^|\/)node(?:\.exe)?$/i.test(normalized) && /\/\.treer\/launchers\/agent\/run\.mjs$/.test(runner || "");
  return (legacy && isDeepStrictEqual(current.args, profile.args)) || (installed && isDeepStrictEqual(current.args.slice(1), profile.args));
}
