import { runProviderSync } from "./process.mjs";

/** The same save/readback/restore transaction for legacy and Controller installs. */
export function profileTransaction(treer, log = console.log) {
  const run = (args) => {
    const result = runProviderSync(treer, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 15000 });
    if (result.error || result.status !== 0) {
      const error = new Error("Treer profile operation failed");
      error.status = result.status; error.stdout = result.stdout; error.stderr = result.stderr;
      throw error;
    }
    return (result.stdout || "").trim();
  };
  function show(name) {
    try { return JSON.parse(run(["agent", "admin", "profile", "show", name])); }
    catch (error) {
      if (error.status && /profile_not_found|not found|no profile/i.test(`${error.stdout || ""} ${error.stderr || ""}`)) return null;
      throw new Error("Unable to read launcher profile");
    }
  }
  function saveProfile(profile, command, cwd, existing) {
    const prefix = ["agent", "admin", "profile"];
    try {
      run(existing ? [...prefix, "update", profile.name, "--description", profile.description, "--cwd", cwd, "--command", command, ...profile.args.flatMap((arg) => ["--arg", arg])]
        : [...prefix, "create", profile.name, "--description", profile.description, "--cwd", cwd, command, "--", ...profile.args]);
    } catch { throw new Error("Unable to save launcher profile"); }
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
    } catch { throw new Error("Unable to restore launcher profile"); }
  }
  return { show, saveProfile, restoreProfile };
}
