import assert from "node:assert/strict";
import test from "node:test";
import type { BackgroundTask } from "../src/model.js";
import { MAX_FINISHED_TASKS, clearFinishedTasks, pruneFinishedTasks } from "../src/task-retention.js";

const HOUR = 60 * 60 * 1000;
const now = Date.parse("2026-10-19T12:00:00.000Z");
const task = (id: string, status: BackgroundTask["status"], endedAgoMs: number | null): BackgroundTask => ({
  id, kind: "shell", description: id, status, startedAt: null, exitCode: null, summary: null,
  endedAt: endedAgoMs === null ? null : new Date(now - endedAgoMs).toISOString(),
});
const map = (...tasks: BackgroundTask[]) => new Map(tasks.map((entry) => [entry.id, entry]));

test("keeps the 20 most recent finished tasks and drops the rest", () => {
  const tasks = map(...Array.from({ length: 35 }, (_, index) => task(`t${index}`, "completed", (35 - index) * 1000))); // t34 is newest
  assert.equal(pruneFinishedTasks(tasks, now), 15);
  assert.equal(tasks.size, MAX_FINISHED_TASKS);
  assert.ok(tasks.has("t34") && tasks.has("t15") && !tasks.has("t14"));
});

test("drops finished tasks older than 24 hours even when under the count limit", () => {
  const tasks = map(task("fresh", "completed", HOUR), task("edge", "failed", 24 * HOUR), task("old", "stopped", 24 * HOUR + 1), task("older", "completed", 40 * HOUR));
  assert.equal(pruneFinishedTasks(tasks, now), 2);
  assert.deepEqual([...tasks.keys()].sort(), ["edge", "fresh"]);
});

test("running tasks are never pruned, however many or however old", () => {
  const tasks = map(...Array.from({ length: 50 }, (_, index) => task(`run${index}`, "running", null)), task("ancient", "running", 100 * HOUR),
    ...Array.from({ length: 30 }, (_, index) => task(`done${index}`, "completed", index * 1000)));
  pruneFinishedTasks(tasks, now);
  assert.equal([...tasks.values()].filter((entry) => entry.status === "running").length, 51);
  assert.equal([...tasks.values()].filter((entry) => entry.status !== "running").length, MAX_FINISHED_TASKS);
});

test("equal end times keep the later arrivals; a missing or invalid end time counts as oldest", () => {
  const stamp = new Date(now).toISOString();
  const tied = Array.from({ length: 25 }, (_, index) => ({ ...task(`n${index}`, "completed", 0), endedAt: stamp }));
  const tasks = map(...tied, { ...task("nodate", "completed", 0), endedAt: null }, { ...task("bad", "completed", 0), endedAt: "not a date" });
  pruneFinishedTasks(tasks, now);
  assert.ok(!tasks.has("nodate") && !tasks.has("bad"));
  assert.ok(tasks.has("n24") && tasks.has("n5") && !tasks.has("n4"));
});

test("clearing removes every finished record and only those", () => {
  const tasks = map(task("a", "running", null), task("b", "completed", 1000), task("c", "failed", 2000), task("d", "stopped", 3000));
  assert.equal(clearFinishedTasks(tasks), 3);
  assert.deepEqual([...tasks.keys()], ["a"]);
  assert.equal(clearFinishedTasks(tasks), 0);
});

test("memory stays bounded over a very long stream of tasks", () => {
  const tasks = new Map<string, BackgroundTask>();
  let peak = 0;
  for (let index = 0; index < 200_000; index++) {
    const id = `t${index}`;
    tasks.set(id, task(id, index % 1000 === 0 ? "running" : "completed", 0));
    if (index % 7 === 0) { pruneFinishedTasks(tasks, now); peak = Math.max(peak, tasks.size); }
  }
  pruneFinishedTasks(tasks, now);
  const running = [...tasks.values()].filter((entry) => entry.status === "running").length;
  assert.equal(running, 200, "every running task survives");
  assert.ok(tasks.size === running + MAX_FINISHED_TASKS, `final size ${tasks.size}`);
  assert.ok(peak <= running + MAX_FINISHED_TASKS + 7, `the map never grew past the limit plus one prune interval (peak ${peak})`);
});

test("pruning only edits the record map: it takes no task output path and never signals a task", () => {
  const tasks = map({ ...task("x", "completed", 99 * HOUR), outputFile: "/nonexistent/should-never-be-touched.output" });
  assert.equal(pruneFinishedTasks(tasks, now), 1); // no throw, no filesystem access to the missing path
});
