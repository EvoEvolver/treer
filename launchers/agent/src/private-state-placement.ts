import { lstatSync, realpathSync } from "node:fs";
import { dirname, isAbsolute, join, relative, sep } from "node:path";
import type { PrivateStateStatus } from "./private-state.js";

const vcsMarkers = [".git", ".hg", ".jj", ".svn", ".bzr", ".sl"];

/** Mirror Core's placement predicate. No caching: a repository may appear
 * after startup. This does not replace the store's no-follow/owner/ACL checks. */
export function privateStatePlacement(cwd: string, privateRoot: string): PrivateStateStatus {
  if (!isAbsolute(privateRoot)) return "agent_private_state_unavailable";
  let physicalCwd: string, physicalRoot: string;
  try {
    physicalCwd = realpathSync.native(cwd);
    physicalRoot = realpathSync.native(privateRoot);
  } catch { return "agent_private_state_unavailable"; }
  for (let directory = physicalRoot;; directory = dirname(directory)) {
    for (const marker of vcsMarkers) {
      try { lstatSync(join(directory, marker)); }
      catch (error) {
        if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
      }
      // Any existing marker (including a dangling link), or inspection failure.
      return "agent_private_state_inside_vcs_worktree";
    }
    if (dirname(directory) === directory) break;
  }
  const inside = relative(physicalCwd, physicalRoot);
  if (isAbsolute(inside) || inside === ".." || inside.startsWith(`..${sep}`)) return "available";
  const first = inside.split(sep)[0];
  return first.startsWith(".") && first !== "." && first !== ".."
    ? "available" : "agent_private_state_inside_cwd";
}
