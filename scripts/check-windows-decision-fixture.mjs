// Source boundary gate for the inert native fixture, not a Rust parser or
// substitute for the non-test Windows compilation check.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = path => readFileSync(new URL(`../${path}`, import.meta.url), 'utf8');
const root = 'crates/treer-platform/src/';
assert.match(read(`${root}lib.rs`), /#\[cfg\(all\(windows, test\)\)\][\s\S]*?mod windows_update_bootstrap;/);
const ownership = `${root}windows_update_bootstrap/checked_files/ownership/`;
const stage = read(`${ownership}consume_stage.rs`);
const pin = read(`${ownership}consume_stage/read_pin.rs`);
assert.match(stage, /\nstruct Stage<'a> \{/);
assert.doesNotMatch(stage, /\bpub(?:\([^\n]*\))?\s+struct Stage/);
assert.match(pin, /struct ReadPin<'a>\(Stage<'a>\);/);
assert.doesNotMatch(pin, /struct ReadPin<'a>\(pub/);
const exports = [...`${stage}\n${pin}`.matchAll(/pub(?:\([^\n]*\))?\s+fn\s+(\w+)\s*(?:<[^>]*>)?\s*\(([\s\S]*?)\)\s*->/g)];
assert.deepEqual(exports.map(match => match[1]).sort(), ['into_read_pin', 'pin_decision', 'validate']);
for (const [, name, argumentsText] of exports) {
  assert.doesNotMatch(argumentsText, /\b(?:Path|PathBuf|Record|File|Stage)\b/, name);
}
assert.match(stage, /Stage::create\(self, bytes, false\)\?\.into_read_pin\(\|\| Ok\(\(\)\)\)/);
assert.match(pin, /if full_identity\(&current\)\? != self\.0\.object \|\| read_bytes\(&current\)\? != self\.0\.bytes/);
console.log('Windows decision fixture source boundary passed (test-only; no exported path/parsed-record/raw-stage constructor).');
