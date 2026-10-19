import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

// Execute the checked-in Rust template, selecting its fixed literals exactly as
// bootstrap_page does. This is test rendering, never a second JS implementation.
export function renderCoreEmbedBootstrap(consoleOrigin: string, reuse = false, prewarm = false) {
  const source = readFileSync('../crates/treer-proxy/src/api/network/embed.rs', 'utf8')
  const template = source.match(/r#"(<!doctype html>[\s\S]*?)"#/i)?.[1]
  assert(template, 'Core bootstrap HTML template missing')
  function literals(name: string) {
    const start = source.indexOf(`let ${name} = if `)
    assert(start >= 0, `Core ${name} branch missing`)
    const end = source.indexOf('\n    };', start)
    assert(end > start, `Core ${name} branch is unterminated`)
    return [...source.slice(start, end).matchAll(/r#"([\s\S]*?)"#/g)].map(match => match[1])
  }
  const probes = literals('v2_probe'), announcements = literals('announce')
  const supportsPrewarm = /let v2_probe = if prewarm/.test(source)
  assert.equal(probes.length, supportsPrewarm ? 2 : 1)
  assert.equal(announcements.length, 2)
  assert(!prewarm || reuse && supportsPrewarm, 'prewarm requires the actual Core prewarm template and reuse')
  const replacements: Record<string, string> = {
    console_js: JSON.stringify(consoleOrigin), script_nonce: 'synthetic-nonce',
    v2_probe: reuse ? probes[prewarm ? 0 : supportsPrewarm ? 1 : 0] : '',
    announce: announcements[reuse ? 0 : 1],
  }
  for (const name of ['finish', 'fail_report']) {
    if (!template.includes(`{${name}}`)) continue
    const choices = literals(name)
    assert.equal(choices.length, 2)
    replacements[name] = choices[prewarm ? 0 : 1]
  }
  return template.replace(/\{(console_js|script_nonce|v2_probe|announce|finish|fail_report)\}/g, (_, name: string) => replacements[name])
    .replaceAll('{{', '{').replaceAll('}}', '}')
}
