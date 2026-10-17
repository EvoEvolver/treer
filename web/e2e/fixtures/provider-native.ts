// Synthetic Core #131 wire fixture; never installs a package or contacts a registry.
export const nativeRecipe = {
  provider_id: 'claude', recipe_id: 'claude-native-npm', recipe_version: 2, recipe_hash: 'c'.repeat(64), platform: 'linux-x86_64', binary_names: ['claude'], supported: true,
  official_docs: 'https://example.com/setup', login_hint: 'Sign in explicitly in your own terminal. Treer does not handle credentials.', node_min: 22,
  expected_version: '2.1.302', node_arch: 'x64', deadline_seconds: 300,
  lifecycle_scripts: { enabled: true, review: { reviewed_at: '2026-10-17', reviewer: 'Fixture static review only', files: [{ member: 'package/install.cjs', sha256: 'd'.repeat(64) }, { member: 'package/package.json', sha256: 'e'.repeat(64) }], scripts: { postinstall: 'node install.cjs', prepare: 'node guard.cjs' }, runs: ['postinstall'], closure: { packages: 2, with_scripts: ['@anthropic-ai/claude-code'] } } },
  package: { name: '@anthropic-ai/claude-code', version: '2.1.302', integrity: 'sha512-' + 'A'.repeat(86) + '==', registry: 'https://registry.npmjs.org/' },
  native_binary: { package: '@anthropic-ai/claude-code-linux-x64', version: '2.1.302', integrity: 'sha512-' + 'B'.repeat(86) + '==', member: 'package/claude', size: 1024, sha256: 'f'.repeat(64), unpacked_size: 2048 },
  plan: [
    { step: 1, kind: 'command', purpose: 'node-arch', executable: '/opt/node/bin/node', args: ['-p', 'process.arch'] },
    { step: 2, kind: 'command', purpose: 'pack', executable: '/opt/node/bin/npm', args: ['pack', '@anthropic-ai/claude-code@2.1.302', '--ignore-scripts', '--pack-destination=/private/job/pack'] },
    { step: 3, kind: 'command', purpose: 'pack-native', executable: '/opt/node/bin/npm', args: ['pack', '@anthropic-ai/claude-code-linux-x64@2.1.302', '--ignore-scripts', '--pack-destination=/private/job/pack-native'] },
    { step: 4, kind: 'verify', purpose: 'integrity', algorithm: 'sha512', expected: 'sha512-' + 'A'.repeat(86) + '==' },
    { step: 5, kind: 'verify', purpose: 'integrity-native', algorithm: 'sha512', expected: 'sha512-' + 'B'.repeat(86) + '==' },
    { step: 6, kind: 'command', purpose: 'install', executable: '/opt/node/bin/npm', args: ['install', '--global', '/private/job/pack/root.tgz', '/private/job/pack-native/native.tgz', '--offline', '--registry=http://127.0.0.1:1/', '--@anthropic-ai:registry=http://127.0.0.1:1/', '--userconfig=/private/job/npmrc', '--globalconfig=/private/job/npmrc-global', '--cache=/private/job/cache'] },
    { step: 7, kind: 'command', purpose: 'prefix', executable: '/opt/node/bin/npm', args: ['prefix', '--global'] },
    { step: 8, kind: 'verify_file', purpose: 'verify-file', rule: 'Verify the pinned file and bin/claude link without execution', relative_path: 'lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe', entry: 'claude', expected_size: 1024, expected_sha256: 'f'.repeat(64), prefix_from_step: 7 },
    { step: 9, kind: 'resolve', purpose: 'check', rule: '<npm prefix --global>/bin/claude --version', binary: 'claude', args: ['--version'], prefix_from_step: 7 },
  ],
}
