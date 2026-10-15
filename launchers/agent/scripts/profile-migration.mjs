import { managedProfile, portableSpec } from "./portable-profile.mjs";
import { isDeepStrictEqual } from "node:util";
import { posix } from "node:path";

/** Pure dry-run planner: names plus exact old launcher argv, never names alone. */
export function profileMigration(profiles, definitions, agents = []) {
  if (!Array.isArray(profiles) || !Array.isArray(agents)) throw new Error("Invalid profile/Agent inventory");
  const changes = [], keep = [];
  for (const definition of definitions) {
    const current = profiles.find(profile => profile.name === definition.name);
    if (!managedProfile(current, definition)) throw new Error(`Customized canonical profile must be preserved explicitly: ${definition.name}`);
    const next = { name: definition.name, ...portableSpec(definition), description: definition.description };
    if (!current || !isDeepStrictEqual({ command: current.command, cwd: current.cwd, args: current.args }, portableSpec(definition))) changes.push({ operation: current ? "update" : "create", name: definition.name, next });
  }
  for (const profile of profiles) {
    if (definitions.some(definition => definition.name === profile.name)) continue;
    const definition = definitions.find(definition => profile.name.startsWith(definition.name + " (") && profile.name.endsWith(")"));
    const suffix = definition && profile.name.slice(definition.name.length + 2, -1);
    const qualified = suffix && /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(suffix);
    const preview = suffix === "React preview f7b76ae mac_m2";
    // A preview is a versioned script, not the stable installed runner.
    const previewArgs = preview && /^node(?:\.exe)?$/i.test(posix.basename(String(profile.command).replaceAll("\\", "/"))) && profile.args?.[0]?.replaceAll?.("\\", "/").endsWith("/f7b76aee3d2202a5dda92794d90a8519c0fa2ba7/scripts/treer-agent.mjs") && isDeepStrictEqual(profile.args.slice(1), definition.args);
    const referenced = agents.some(agent => agent.launch_profile_id === profile.profile_id && profile.profile_id || agent.profile_id === profile.profile_id && profile.profile_id);
    if (definition && (qualified || preview) && (managedProfile(profile, definition) || previewArgs) && !referenced) changes.push({ operation: "delete", name: profile.name });
    else keep.push(profile.name);
  }
  return { changes, keep, agents_checked: agents.length, startup_semantics: "Profiles are copied into Agent startup specifications; changing/deleting a profile does not alter those copies" };
}
