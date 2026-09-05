# Optional ACP Presentation

Presentation adapters in this directory are never installed or selected by
default. Each adapter must be pinned, built during an explicit `apply.sh`
operation, and represented by a separately named Agent launch profile.

Remote Codex compatibility is compiled behind the runtime's
`remote-codex-ui` feature. Its routes and state projection live under
`runtime/src/optional_ui/`; the headless runtime does not compile that module.
