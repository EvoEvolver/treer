import assert from "node:assert/strict";
import test from "node:test";

import {
  canForkSession,
  createForkedAgent,
  forkAgentName,
  forkLaunchArgs,
  normalizePort,
  promptOptions,
  serviceName,
  snapshotFromContext,
} from "./extension.mjs";

test("normalizes the configured loopback port", () => {
  assert.equal(normalizePort(undefined), 4180);
  assert.equal(normalizePort("0"), 0);
  assert.equal(normalizePort("4312"), 4312);
  assert.throws(() => normalizePort("-1"), /integer from 0 to 65535/);
});

test("derives a stable service name without exposing the full agent id", () => {
  assert.equal(serviceName("ag_0123456789abcdef"), "pi-ui-456789abcdef");
  assert.equal(serviceName(undefined), "pi-ui-local");
});

test("requires explicit delivery while Pi is working", () => {
  assert.equal(promptOptions(true, "prompt"), undefined);
  assert.deepEqual(promptOptions(false, "steer"), { deliverAs: "steer" });
  assert.deepEqual(promptOptions(false, "followUp"), { deliverAs: "followUp" });
  assert.throws(() => promptOptions(false, "prompt"), /choose steer or follow-up/);
});

test("builds unique fork names within Treer's display-name limit", () => {
  assert.equal(forkAgentName("pi-ui", "abc-123"), "pi-ui-fork-abc-123");
  assert.equal([...forkAgentName("x".repeat(100), "abc-123")].length, 80);
});

test("builds a shell launch that forks Pi and reloads the UI extension", () => {
  assert.deepEqual(forkLaunchArgs({
    cwd: "projects/demo",
    extension: "/machine/projects/demo/apps/pi-ui/extension.mjs",
    name: "pi-fork-one",
    sessionFile: "/machine/sessions/source.jsonl",
  }), [
    "agent", "admin", "create",
    "--machine", "self",
    "--kind", "shell",
    "--name", "pi-fork-one",
    "--cwd", "projects/demo",
    "--",
    "env", "PI_UI_PORT=0",
    "pi", "--fork", "/machine/sessions/source.jsonl",
    "--extension", "/machine/projects/demo/apps/pi-ui/extension.mjs",
    "--approve",
  ]);
});

test("requires a persisted conversation before forking", () => {
  assert.equal(canForkSession({
    sessionManager: { getBranch: () => [], getSessionFile: () => "/sessions/empty.jsonl" },
  }), false);
  assert.equal(canForkSession({
    sessionManager: {
      getBranch: () => [{ type: "message", message: { role: "user" } }],
      getSessionFile: () => "/sessions/ready.jsonl",
    },
  }), true);
});

test("creates a forked Agent from the current saved session", async () => {
  const calls = [];
  const run = async (command, args) => {
    calls.push([command, args]);
    if (args[0] === "agent" && args[1] === "show") {
      return { stdout: JSON.stringify({ cwd: "treer", name: "pi-ui" }) };
    }
    return { stdout: JSON.stringify({ agent_id: "ag_child", name: "pi-ui-fork-test" }) };
  };
  const child = await createForkedAgent({
    isIdle: () => true,
    sessionManager: {
      getBranch: () => [{ type: "message", message: { role: "user" } }],
      getSessionFile: () => "/sessions/source.jsonl",
    },
  }, {
    extension: "/treer/apps/pi-ui/extension.mjs",
    run,
    suffix: "test",
  });
  assert.equal(child.agent_id, "ag_child");
  assert.deepEqual(calls[0], ["treer", ["agent", "show", "self"]]);
  assert.equal(calls[1][0], "treer");
  assert.deepEqual(calls[1][1], forkLaunchArgs({
    cwd: "treer",
    extension: "/treer/apps/pi-ui/extension.mjs",
    name: "pi-ui-fork-test",
    sessionFile: "/sessions/source.jsonl",
  }));
});

test("projects the active Pi session into the browser snapshot", () => {
  const entries = [{ type: "message", id: "one", message: { role: "user", content: "hello" } }];
  const context = {
    cwd: "/workspace/project",
    model: { id: "model-1", name: "Model One", provider: "test" },
    thinkingLevel: "medium",
    getContextUsage: () => ({ tokens: 10, contextWindow: 100, percent: 10 }),
    sessionManager: {
      getBranch: () => entries,
      getSessionFile: () => "/tmp/session.jsonl",
      getSessionId: () => "session-1",
      getSessionName: () => "Demo",
    },
  };
  const snapshot = snapshotFromContext(context, {
    activeTools: new Map([["tool-1", { id: "tool-1", status: "running" }]]),
    busy: true,
    error: null,
    forking: false,
    lastFork: null,
    liveMessage: null,
    port: 4180,
  });
  assert.equal(snapshot.session.name, "Demo");
  assert.equal(snapshot.model.provider, "test");
  assert.equal(snapshot.busy, true);
  assert.equal(snapshot.forking, false);
  assert.equal(snapshot.canFork, true);
  assert.deepEqual(snapshot.entries, entries);
  assert.equal(snapshot.activeTools[0].id, "tool-1");
});
