import assert from "node:assert/strict";
import test from "node:test";

import piUiExtension, {
  canForkSession,
  createForkedAgent,
  forkAgentName,
  forkLaunchArgs,
  normalizePort,
  promptOptions,
  registerTreerInterface,
  serviceName,
  snapshotFromContext,
  transcriptEntries,
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

test("registers the Pi server as an Agent Interface Server", async () => {
  const calls = [];
  await registerTreerInterface(4180, "pi_instance", {
    run: async (command, args) => calls.push([command, args]),
  });
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], "treer");
  assert.deepEqual(calls[0][1].slice(0, 8), [
    "interface", "register", "--port", "4180",
    "--instance-id", "pi_instance", "--ui-path", "/",
  ]);
  assert.equal(calls[0][1].filter((value) => value === "--capability").length, 6);
});

test("projects Pi entries into stable Treer transcript envelopes", () => {
  const context = {
    sessionManager: {
      getSessionId: () => "session-1",
      getBranch: () => [
        { type: "message", id: "entry-1", timestamp: "2026-08-24T12:00:00Z", message: { role: "user", content: "hello" } },
        { type: "model_change", model: "test" },
      ],
    },
  };
  assert.deepEqual(transcriptEntries(context, 0, 2), [
    {
      id: "entry-1",
      kind: "message",
      role: "user",
      content: "hello",
      created_at: "2026-08-24T12:00:00Z",
    },
    {
      id: "session-1:1",
      kind: "model_change",
      role: null,
      content: { type: "model_change", model: "test" },
      created_at: null,
    },
  ]);
});

test("serves the AIS contract and deduplicates prompt operations", async () => {
  const previousPort = process.env.PI_UI_PORT;
  const previousRegister = process.env.PI_UI_AUTO_REGISTER;
  const previousAgent = process.env.TREER_AGENT_ID;
  process.env.PI_UI_PORT = "0";
  process.env.PI_UI_AUTO_REGISTER = "0";
  delete process.env.TREER_AGENT_ID;
  const handlers = new Map();
  const prompts = [];
  const pi = {
    on: (name, handler) => handlers.set(name, handler),
    sendUserMessage: (message) => prompts.push(message),
  };
  const context = {
    cwd: "/workspace",
    isIdle: () => true,
    sessionManager: {
      getBranch: () => [{ type: "message", id: "one", message: { role: "user", content: "hello" } }],
      getSessionFile: () => "/tmp/session.jsonl",
      getSessionId: () => "session-1",
      getSessionName: () => "Test",
    },
  };
  const control = piUiExtension(pi);
  try {
    await handlers.get("session_start")({}, context);
    const base = `http://127.0.0.1:${control.port}`;
    const manifest = await (await fetch(`${base}/v1/manifest`)).json();
    assert.equal(manifest.protocol, "treer.agent-interface/v1");
    assert.equal(manifest.instance_id, control.instanceId);

    const transcript = await (await fetch(`${base}/v1/transcript?limit=10`)).json();
    assert.equal(transcript.entries[0].id, "one");
    assert.equal(transcript.entries[0].content, "hello");

    const request = {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation_id: "cmd-one", text: "do work", mode: "prompt" }),
    };
    assert.equal((await fetch(`${base}/v1/prompts`, request)).status, 202);
    assert.equal((await fetch(`${base}/v1/prompts`, request)).status, 202);
    assert.deepEqual(prompts, ["do work"]);
  } finally {
    await handlers.get("session_shutdown")();
    if (previousPort === undefined) delete process.env.PI_UI_PORT;
    else process.env.PI_UI_PORT = previousPort;
    if (previousRegister === undefined) delete process.env.PI_UI_AUTO_REGISTER;
    else process.env.PI_UI_AUTO_REGISTER = previousRegister;
    if (previousAgent === undefined) delete process.env.TREER_AGENT_ID;
    else process.env.TREER_AGENT_ID = previousAgent;
  }
});
