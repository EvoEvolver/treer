import { runProviderSync } from "./process.mjs";

// Never relay arbitrary stderr/message text: it can contain credentials, paths
// or user-controlled profile descriptions. Preserve only known diagnostic codes.
const diagnosticCodes = new Set(["cli_failed", "launch_profile_not_found", "launch_profile_exists", "unauthorized", "forbidden", "permission_denied", "cluster_error", "invalid_request", "machine_not_found"]);
const spawnCodes = new Set(["ENOENT", "EACCES", "EPERM", "EINVAL", "ETIMEDOUT", "ENOBUFS"]);
export function profileFailureSummary(result) {
  let code;
  for (const output of [result.stderr, result.stdout]) {
    try {
      const value = JSON.parse(output || "null")?.error?.code;
      if (diagnosticCodes.has(value)) { code = value; break; }
    } catch { /* Non-JSON diagnostics are deliberately not copied. */ }
  }
  if (!code && spawnCodes.has(result.error?.code)) code = result.error.code;
  const status = Number.isInteger(result.status) ? `exit ${result.status}` : "spawn failed";
  return `Treer profile operation failed (${status}${code ? `; ${code}` : "; details withheld"})`;
}

/** The same save/readback/restore transaction for legacy and Controller installs. */
export function profileTransaction(treer, log = console.log) {
  const run = (args) => {
    let result;
    try { result = runProviderSync(treer, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 15000 }); }
    catch (error) { throw new Error(profileFailureSummary({ error })); }
    if (result.error || result.status !== 0) {
      const error = new Error(profileFailureSummary(result));
      error.status = result.status;
      error.profileNotFound = [result.stdout, result.stderr].some((output) => {
        try { return JSON.parse(output || "null")?.error?.code === "launch_profile_not_found"; }
        catch { return String(output || "").trim() === "profile_not_found"; }
      });
      throw error;
    }
    return (result.stdout || "").trim();
  };
  function show(name) {
    try { return JSON.parse(run(["agent", "admin", "profile", "show", name])); }
    catch (error) {
      if (error.status && error.profileNotFound) return null;
      throw new Error(`Unable to read launcher profile: ${error.message.startsWith("Treer profile operation failed (") ? error.message : "invalid profile response"}`);
    }
  }
  function saveProfile(profile, command, cwd, existing) {
    const prefix = ["agent", "admin", "profile"];
    try {
      run(existing ? [...prefix, "update", profile.name, "--description", profile.description, "--cwd", cwd, "--command", command, ...profile.args.flatMap((arg) => ["--arg", arg])]
        : [...prefix, "create", profile.name, "--description", profile.description, "--cwd", cwd, command, "--", ...profile.args]);
    } catch (error) { throw new Error(`Unable to save launcher profile: ${error.message}`); }
    const saved = show(profile.name);
    if (!saved || saved.command !== command || saved.cwd !== cwd || JSON.stringify(saved.args) !== JSON.stringify(profile.args)) throw new Error(`Profile verification failed: ${profile.name}`);
    log(`Verified profile: ${profile.name}`);
  }
  function restoreProfile({ name, previous }) {
    const prefix = ["agent", "admin", "profile"];
    try {
      if (!previous) { run([...prefix, "delete", name]); return; }
      const args = previous.args ?? [];
      run([...prefix, "update", previous.profile_id ?? name, "--description", previous.description ?? "", "--cwd", previous.cwd, "--command", previous.command,
        ...(args.length ? args.flatMap((arg) => ["--arg", arg]) : ["--clear-args"])]);
    } catch (error) { throw new Error(`Unable to restore launcher profile: ${error.message}`); }
  }
  // Installation is also an operator operation; whoami requires an Agent.
  // Bind discovery to the enrolled identity already verified by the Controller.
  function machine(serverId) {
    const machines = JSON.parse(run(["machine", "list"]));
    const matches = Array.isArray(machines) ? machines.filter((item) => item?.server_id === serverId) : [];
    if (!serverId || matches.length !== 1) throw new Error("Unable to verify launcher machine name");
    return matches[0];
  }
  return { show, saveProfile, restoreProfile, machine };
}
