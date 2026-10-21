import { readFileSync } from 'node:fs'

// Extract the actual Rust-owned template and every selected raw-string branch.
// Missing branches fail the test; there is no substitute bootstrap implementation.
export function renderEmbedBootstrap(consoleOrigin: string, reuse: boolean, prewarm = false) {
  const source = readFileSync('../crates/treer-proxy/src/api/network/embed.rs', 'utf8')
  const requireMatch = (pattern: RegExp, name: string) => {
    const match = source.match(pattern)
    if (!match) throw Error(`Rust embed template branch missing: ${name}`)
    return match
  }
  const template = requireMatch(/r#"(<!doctype html>[\s\S]*?)"#/i, 'html')[1]
  const probes = requireMatch(/let v2_probe = if prewarm \{\s*r#"([\s\S]*?)"#\s*\} else if reuse \{\s*r#"([\s\S]*?)"#/, 'probes')
  const branch = (name: string, condition: string) => requireMatch(new RegExp(`let ${name} = if ${condition} \\{\\s*r#"([\\s\\S]*?)"#\\s*\\} else \\{\\s*r#"([\\s\\S]*?)"#`), name)
  const hidden = reuse && prewarm
  const announce = branch('announce', 'reuse')
  const finish = branch('finish', 'prewarm')
  const failure = branch('fail_report', 'prewarm')
  return template.replaceAll('{console_js}', JSON.stringify(consoleOrigin))
    .replaceAll('{script_nonce}', 'local-availability-test')
    .replaceAll('{v2_probe}', reuse ? probes[hidden ? 1 : 2] : '')
    .replaceAll('{announce}', announce[reuse ? 1 : 2])
    .replaceAll('{finish}', finish[hidden ? 1 : 2])
    .replaceAll('{fail_report}', failure[hidden ? 1 : 2])
    .replaceAll('{{', '{').replaceAll('}}', '}')
}
