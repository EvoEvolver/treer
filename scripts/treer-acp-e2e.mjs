import { spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const treer = resolve(root, "target/debug/treer");
const treerAcp = resolve(root, "target/debug/treer-acp");
const fixtureSrc = resolve(
  root,
  "crates/treer-acp/tests/fixtures/ui-dist/index.html",
);

function fail(message) {
  throw new Error(message);
}

async function readJson(url, init) {
  const response = await fetch(url, init);
  const text = await response.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }
  return { response, body };
}

async function waitIdle(base, timeoutMs = 8000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const { body } = await readJson(`${base}/v1/status`);
    if (body.status === "idle" && body.busy === false) return body;
    await new Promise((resolveWait) => setTimeout(resolveWait, 40));
  }
  fail(`timed out waiting for idle at ${base}`);
}

function spawnLogged(command, args, options) {
  const child = spawn(command, args, {
    ...options,
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stderr.on("data", (chunk) => process.stderr.write(chunk));
  return child;
}

function waitForListen(child) {
  return new Promise((resolveListen, reject) => {
    let done = false;
    let stdout = "";
    const finish = (fn) => {
      if (done) return;
      done = true;
      child.stdout.off("data", onData);
      fn();
    };
    const onData = (chunk) => {
      stdout += chunk.toString();
      const match = stdout.match(/listening on 127\.0\.0\.1:(\d+)/);
      if (match) {
        finish(() => resolveListen(Number(match[1])));
      }
    };
    child.stdout.on("data", onData);
    child.once("exit", (code, signal) => {
      finish(() =>
        reject(
          new Error(
            `treer-acp exited before listen (code=${code} signal=${signal}): ${stdout}`,
          ),
        ),
      );
    });
    setTimeout(() => {
      finish(() =>
        reject(new Error(`timed out waiting for treer-acp listen: ${stdout}`)),
      );
    }, 20_000);
  });
}

async function runTreer(args, env) {
  const child = spawnLogged(treer, args, { env, cwd: root });
  let stdout = "";
  child.stdout.on("data", (chunk) => {
    stdout += chunk.toString();
  });
  const code = await new Promise((resolveCode) => child.once("close", resolveCode));
  if (code !== 0) {
    fail(`treer ${args.join(" ")} exited ${code}\n${stdout}`);
  }
  return stdout;
}

const work = await mkdtemp(join(tmpdir(), "treer-acp-e2e-"));
const uiHome = join(work, "ui-home");
const cwd = join(work, "cwd");
const fixture = join(work, "ui-src");
await mkdir(cwd, { recursive: true });
await mkdir(fixture, { recursive: true });
await writeFile(join(fixture, "index.html"), await readFile(fixtureSrc, "utf8"));

const env = {
  ...process.env,
  TREER_UI_HOME: uiHome,
  AIS_AUTO_REGISTER: "0",
};

try {
  const installOut = await runTreer(["ui", "install", "--dir", fixture], env);
  const installed = JSON.parse(installOut);
  if (!installed.installed) fail(`ui install did not report installed: ${installOut}`);
  if (!installed.dist_path) fail(`ui install missing dist_path: ${installOut}`);

  const showOut = await runTreer(["ui", "show"], env);
  const shown = JSON.parse(showOut);
  if (shown.installed !== true) fail(`ui show installed=false: ${showOut}`);

  const child = spawnLogged(
    treerAcp,
    ["--fake", "--cwd", cwd, "--agent-id", "e2e", "--port", "0"],
    { env, cwd },
  );
  try {
    const port = await waitForListen(child);
    const base = `http://127.0.0.1:${port}`;

    const { response: manifestRes, body: manifest } = await readJson(
      `${base}/v1/manifest`,
    );
    if (!manifestRes.ok) fail(`/v1/manifest HTTP ${manifestRes.status}`);
    if (manifest.protocol !== "treer.agent-interface/v1") {
      fail(`unexpected protocol ${manifest.protocol}`);
    }
    const caps = manifest.capabilities ?? [];
    for (const needed of [
      "prompt.submit",
      "transcript.read",
      "state.observe",
      "abort",
    ]) {
      if (!caps.includes(needed)) fail(`missing capability ${needed}`);
    }
    if (manifest.ui_path !== "/") fail(`expected ui_path /, got ${manifest.ui_path}`);

    const promptRes = await fetch(`${base}/v1/prompts`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ operation_id: "e2e-1", text: "quick-success" }),
    });
    if (promptRes.status !== 202) fail(`prompt HTTP ${promptRes.status}`);
    await waitIdle(base);

    const { body: transcript } = await readJson(`${base}/v1/transcript?limit=10`);
    const ids = (transcript.entries ?? []).map((entry) => entry.id);
    if (!ids.includes("e2e-1:user")) fail(`transcript missing turn: ${JSON.stringify(transcript)}`);

    const abortRes = await fetch(`${base}/v1/abort`, { method: "POST" });
    if (abortRes.status !== 202) fail(`abort HTTP ${abortRes.status}`);

    const writeRes = await fetch(`${base}/v1/files?path=hello.txt`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ content: "hi" }),
    });
    if (!writeRes.ok) fail(`file write HTTP ${writeRes.status}`);
    const { body: file } = await readJson(`${base}/v1/files?path=hello.txt`);
    if (file.content !== "hi") fail(`file read mismatch ${JSON.stringify(file)}`);

    const escapeRes = await fetch(`${base}/v1/files/tree?path=..`);
    if (escapeRes.status !== 400) fail(`expected ../ tree to be 400, got ${escapeRes.status}`);
    const escapeWrite = await fetch(`${base}/v1/files?path=../escape.txt`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ content: "no" }),
    });
    if (escapeWrite.status !== 400) {
      fail(`expected ../ write to be 400, got ${escapeWrite.status}`);
    }

    const uiRes = await fetch(`${base}/`);
    const uiHtml = await uiRes.text();
    if (!uiRes.ok) fail(`GET / HTTP ${uiRes.status}`);
    if (!uiHtml.includes("hello treer-acp ui")) {
      fail(`GET / missing fixture html: ${uiHtml}`);
    }

    console.log("treer-acp e2e passed");
  } finally {
    child.kill("SIGTERM");
    await new Promise((resolveClose) => child.once("close", resolveClose));
  }
} finally {
  await rm(work, { recursive: true, force: true });
}
