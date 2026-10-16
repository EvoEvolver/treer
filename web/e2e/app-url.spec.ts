import { test, expect } from '@playwright/test'
import { appOwnUrl } from '../src/lib/app-url'

const blocked = ['https://console.example.test', 'https://proxy.example.test']
const url = (public_url: unknown) => appOwnUrl({ public_url } as { public_url?: string }, blocked)

test('appOwnUrl accepts only an https DNS name that is not the Console or the Proxy', () => {
  expect(url('https://svc-abc123.apps.example.test/')).toBe('https://svc-abc123.apps.example.test/')
  expect(url('https://SVC-ABC.apps.example.test:8443/x?y=1')).toBe('https://svc-abc.apps.example.test:8443/x?y=1')
  for (const bad of [undefined, null, 42, '', 'svc-abc', 'http://svc-abc.apps.example.test/', 'https://user:pw@svc-abc.apps.example.test/', 'https://:pw@svc-abc.apps.example.test/',
    'javascript:alert(1)', 'data:text/html,x', 'https://localhost/', 'https://app.localhost/', 'https://10.0.0.5/', 'https://[::1]/', 'https://intranet/', 'https://-bad.example.test/',
    'https://console.example.test/app', 'https://proxy.example.test/', 'https://' + 'a'.repeat(3000) + '.example.test/']) expect(url(bad), String(bad).slice(0, 40)).toBeNull()
})
