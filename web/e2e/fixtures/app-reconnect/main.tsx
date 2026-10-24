import React, { useState } from 'react'
import { createRoot } from 'react-dom/client'
import { WorkloadSurface } from '../../../src/components/workload-surface'
import { loadRuntimeConfig, type Agent, type AppDeployment, type Machine } from '../../../src/lib/api'
import { setLocale } from '../../../src/i18n'
import '../../../src/index.css'

await loadRuntimeConfig()
function Fixture() {
  const [generation, next] = useState(1)
  const [visible, show] = useState(true)
  const [theme, changeTheme] = useState<'light' | 'dark'>('light')
  const [lang, language] = useState<'en' | 'zh'>('en')
  const agent = { agent_id: 'app-runtime', server_id: 'server', status: 'idle', run_generation: generation } as Agent
  const app = { app_id: 'app', hostname: 'app.internal', status: 'running' } as AppDeployment
  return <div className={`${theme} bg-background text-foreground`} style={{ minHeight: '100vh' }}>
    <nav><button onClick={() => next(generation + 1)}>New generation</button><button onClick={() => show(!visible)}>Toggle tab</button><button onClick={() => changeTheme(theme === 'light' ? 'dark' : 'light')}>Theme</button><button onClick={() => { const l = lang === 'en' ? 'zh' : 'en'; setLocale(l); language(l) }}>Language</button></nav>
    <main style={{ height: '80vh', display: visible ? 'block' : 'none' }}><WorkloadSurface workspaceId="workspace" agent={agent} app={app} machine={{ status: 'online' } as Machine} path="Demo" tab={{ id: 'app', live: true, used: 1, revision: 0, logs: false, appId: 'app' }} visible={visible} mobile={false} theme={theme} ctrlArmed={false} embedMode="legacy" transformInput={v => v} onToggleCtrl={() => {}} onCopy={() => {}} onLogs={() => {}} /></main>
  </div>
}
createRoot(document.getElementById('root')!).render(<Fixture />)
