#!/usr/bin/env node
// Default is read-only. Run apply only after installing shims on every machine.
import { closeSync, fsyncSync, mkdtempSync, openSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isDeepStrictEqual } from "node:util";
import { profileMigration } from "./profile-migration.mjs";
import { profileTransaction, profileFailureSummary } from "./profile-transaction.mjs";
import { runProviderSync } from "./process.mjs";
import { protectDirectory, syncDirectory } from "./private-state.mjs";
const args = process.argv.slice(2), apply = args.includes("--apply");
if (args.some(arg => !["--apply", "--dry-run"].includes(arg)) || apply && args.includes("--dry-run")) throw new Error("Usage: migrate-profiles.mjs [--dry-run | --apply]");
const treer = process.env.AGENT_UI_TREER_BIN || "treer";
function read(args) {
  const result = runProviderSync(treer, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 20000 });
  if (result.error || result.status !== 0) throw new Error(profileFailureSummary(result));
  return JSON.parse(result.stdout);
}
const profiles = read(["agent", "admin", "profile", "list"]).profiles;
const agents = read(["--format", "json", "agent", "ls", "-R", "/"]).agents;
const definitions = JSON.parse(readFileSync(new URL("../profiles.json", import.meta.url))).profiles;
const plan = profileMigration(profiles, definitions, agents);
console.log(JSON.stringify({ dry_run: !apply, ...plan }, null, 2));
if (apply) {
  // The OS creates a unique private directory; no pre-existing path is reused.
  // Preserve original JSON before even the first workspace mutation. Do not
  // print the contents: commands/descriptions may contain sensitive data.
  const directory = mkdtempSync(join(tmpdir(), "treer-profile-backup-"));
  protectDirectory(directory);
  const backup = join(directory, "profiles.json"), fd = openSync(backup, "wx", 0o600);
  try { writeFileSync(fd, JSON.stringify({ schema: "treer.profile-backup/v1", createdAt: new Date().toISOString(), profiles, plan }, null, 2) + "\n"); fsyncSync(fd); }
  finally { closeSync(fd); }
  syncDirectory(directory);
  console.error(`Profile backup: ${backup}`);
  const transaction = profileTransaction(treer), changed = [];
  try {
    for (const change of plan.changes) {
      const previous = profiles.find(profile => profile.name === change.name) ?? null;
      // Re-read immediately before mutation to avoid overwriting a concurrent edit.
      const actual = transaction.show(change.name);
      if (!isDeepStrictEqual(actual, previous)) throw new Error("Profile inventory changed; run the dry-run again");
      changed.push({ name: change.name, previous });
      if (change.operation === "delete") transaction.deleteProfile(change.name);
      else transaction.saveProfile(change.next, change.next.command, change.next.cwd, actual);
    }
  } catch (error) {
    let failed = false;
    for (const original of changed.reverse()) {
      try {
        if (original.previous && !transaction.show(original.name)) transaction.saveProfile(original.previous, original.previous.command, original.previous.cwd, null);
        else transaction.restoreProfile(original);
      } catch { failed = true; }
    }
    if (failed) throw new Error("Profile migration rollback incomplete; inspect workspace profiles before retrying");
    throw error;
  }
}
