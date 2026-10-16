import { test, expect } from '@playwright/test'
import { agentCreationFailure } from '../src/lib/agent-creation-failure'

test('creation failure details require the exact machine, safe identity and explicit outcome', () => {
  const details = { agent_id: 'ag_reserved', machine_id: 'srv-a', creation_state: 'cleanup_failed', command: 'claude' }
  expect(agentCreationFailure(details, 'srv-a')).toEqual(details)
  for (const value of [undefined, null, {}, { ...details, machine_id: 'srv-b' }, { ...details, agent_id: '../ag_other' }, { ...details, creation_state: 'running' }]) expect(agentCreationFailure(value, 'srv-a')).toBeNull()
  for (const command of ['/usr/bin/claude', '..', 'claude.', 'claude --flag', '<script>', 'x'.repeat(129)]) expect(agentCreationFailure({ ...details, command }, 'srv-a')?.command).toBeUndefined()
})
