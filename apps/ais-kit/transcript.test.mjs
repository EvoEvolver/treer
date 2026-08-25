import assert from "node:assert/strict";
import test from "node:test";

import {
  groupTranscriptTurns,
  parseTranscriptPageQuery,
  transcriptPageFromEntries,
} from "./transcript.mjs";

test("groups a conversation turn from a user prompt through the next prompt", () => {
  const entries = [
    { kind: "model_change", type: "model_change" },
    { kind: "message", type: "message", role: "user", id: "u1" },
    { kind: "message", type: "message", role: "tool", id: "t1" },
    { kind: "message", type: "message", role: "assistant", id: "a1" },
    { kind: "compaction", type: "compaction" },
    { kind: "message", type: "message", role: "user", id: "u2" },
    { kind: "message", type: "message", role: "assistant", id: "a2" },
  ];
  const turns = groupTranscriptTurns(entries);
  assert.equal(turns.length, 2);
  assert.deepEqual(turns[0].map((entry) => entry.id ?? entry.kind), [
    "model_change",
    "u1",
    "t1",
    "a1",
    "compaction",
  ]);
  assert.deepEqual(turns[1].map((entry) => entry.id), ["u2", "a2"]);
});

test("pages one conversation turn at a time", () => {
  const entries = [
    { kind: "message", type: "message", role: "user", id: "u1", content: "first" },
    { kind: "message", type: "message", role: "assistant", id: "a1", content: "ok" },
    { kind: "message", type: "message", role: "user", id: "u2", content: "second" },
    { kind: "message", type: "message", role: "assistant", id: "a2", content: "done" },
  ];
  const first = transcriptPageFromEntries(entries, 0, 1);
  assert.equal(first.page, 0);
  assert.equal(first.page_count, 2);
  assert.equal(first.next_page, 1);
  assert.deepEqual(first.entries.map((entry) => entry.id), ["u1", "a1"]);
  const second = transcriptPageFromEntries(entries, 1, 1);
  assert.equal(second.next_page, null);
  assert.deepEqual(second.entries.map((entry) => entry.id), ["u2", "a2"]);
  assert.deepEqual(parseTranscriptPageQuery(new URLSearchParams("cursor=3")), {
    page: 3,
    limit: 1,
  });
});
