import test from 'node:test'
import assert from 'node:assert/strict'
import { observePrepareBinding, projectObservedOperation, observedOperationForDocument } from './controller-update-prepare-observer.mjs'
const expected = { origin: 'http://127.0.0.1:12345', path: '/api/workspaces/ws/machines/m/controller-update/prepare', targetId: 'new' }
const operationId = '11111111-1111-4111-8111-111111111111'
function response({ url = expected.origin + expected.path, method = 'POST', ok = true, body = { target: { target_id: 'new' }, operation_id: operationId }, fail = false } = {}) {
  return { url: () => url, request: () => ({ method: () => method }), ok: () => ok, json: async () => { if (fail) throw Error('PRIVATE_BODY_TOKEN'); return body } }
}
test('matching successful response binds exact operation', async () => {
  assert.deepEqual(await observePrepareBinding(response(), expected), { status: 'bound', operationId })
})
test('unavailable observation body yields fixed classification without raw error', async () => {
  assert.deepEqual(await observePrepareBinding(response({ fail: true }), expected), { status: 'body_unavailable' })
})
test('target mismatch and absent operation are distinct from body failure', async () => {
  assert.deepEqual(await observePrepareBinding(response({ body: { target: { target_id: 'other' }, operation_id: operationId } }), expected), { status: 'target_mismatch' })
  assert.deepEqual(await observePrepareBinding(response({ body: { target: { target_id: 'new' } } }), expected), { status: 'operation_invalid' })
})
test('unrelated origin/path/method and unsuccessful responses are not trusted', async () => {
  for (const change of [{ url: 'http://127.0.0.1:12346' + expected.path }, { url: expected.origin + expected.path + '/other' }, { method: 'GET' }, { ok: false }]) {
    assert.deepEqual(await observePrepareBinding(response(change), expected), { status: 'ignored' })
  }
})

test('malformed, non-v4 and non-RFC variant operation UUIDs are refused', async () => {
  for (const id of ['not-a-uuid', '11111111-1111-1111-8111-111111111111', '11111111-1111-4111-7111-111111111111']) {
    assert.deepEqual(await observePrepareBinding(response({ body: { target: { target_id: 'new' }, operation_id: id } }), expected), { status: 'operation_invalid' })
  }
})

const ident = c => ({ release_sha: c.repeat(40), artifact_source_sha: c.repeat(40), controller_sha256: c.repeat(64), cli_sha256: c.repeat(64) })
const final = () => ({ operation_id: operationId, current: ident('a'), target: { target_id: 'new', identity: ident('b'), platform: 'linux-x86_64', version_label: 'new' }, state: 'succeeded', phase: 'complete', observation: 'available', observed_identity: ident('b'), restored_identity: null, health: 'healthy', receipt_revision: 1, created_at: '2026-10-25T01:00:00Z', updated_at: '2026-10-25T01:01:00Z', observed_at: '2026-10-25T01:01:00Z', finished_at: '2026-10-25T01:01:00Z', error_code: null })
test('full operation projection strips unknown/token fields and preserves required comparison fields', () => {
  assert.deepEqual(projectObservedOperation({ ...final(), token: 'PRIVATE', target: { ...final().target, token: 'PRIVATE', identity: { ...ident('b'), secret: 'PRIVATE' } } }), final())
})
test('invalid full operation fields or success semantics cannot supply evidence', () => {
  for (const change of [{ health: 'unknown' }, { observed_identity: ident('c') }, { updated_at: 'invalid' }, { receipt_revision: -1 }, { current: { ...ident('a'), controller_sha256: 'bad' } }, { finished_at: null }]) assert.throws(() => projectObservedOperation({ ...final(), ...change }), /invalid observation/)
  const absent = final(); delete absent.error_code
  assert.throws(() => projectObservedOperation(absent), /invalid observation/)
})
test('old-document operation bridge is refused even with an otherwise valid operation', () => {
  const message = { documentId: 'old', kind: 'operations', value: { status: 'bound', operations: [final()] } }
  assert.equal(observedOperationForDocument(message, 'fresh', operationId), null)
  assert.deepEqual(observedOperationForDocument(message, 'old', operationId), final())
})
