import { createElement, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { useAgentEmbedPrewarm } from '../../src/components/use-agent-embed-prewarm'

// A closed browser harness for caller authentication generations. No credential
// or production session is provided, persisted or inferred from a user ID.
export function mountPrewarmSession() {
  function Harness() {
    const [identity, setIdentity] = useState<object>({})
    const [workspace, setWorkspace] = useState('ws-fixture')
    const [enabled, setEnabled] = useState(true)
    const controls = useAgentEmbedPrewarm('same-user', workspace, identity, enabled,
      [1, 2].map(i => ({ id: `ag_${i.toString(16).padStart(32, '0')}`, origin: `https://ag-${i.toString(16).padStart(32, '0')}.agents.example.test`, revision: '1' })), [])
    Object.assign(window, { prewarmSession: {
      enter: controls.enter, cancel: controls.cancel,
      replaceSession: () => setIdentity({}),
      replaceWorkspace: () => setWorkspace('ws-other'),
      disable: () => setEnabled(false),
    } })
    return createElement('p', null, 'Synthetic prewarm session harness')
  }
  createRoot(document.getElementById('root')!).render(createElement(Harness))
}
