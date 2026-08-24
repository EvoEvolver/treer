# Codex UI

Codex UI is Treer's single-Agent browser interface for Codex. One command Agent
owns one `codex app-server` process, one Codex thread, one AIS registration, and
one embedded page. There is no thread list or session switcher in the UI.

The private HTTP listener exposes the browser page plus
`treer.agent-interface/v1` prompt, transcript, state, and abort operations.
Treer reaches it through the Agent network bridge; no service record or
published port is required.

From a Treer-managed Agent in this checkout:

```bash
./apps/codex-ui/scripts/apply.sh --name codex-ui
```

The script installs the local Node dependencies, saves the `Codex + UI` launch
profile, creates the first command Agent, and waits for its verified Interface
descriptor. Additional Agents can be created from that profile.

For local development outside a managed Agent:

```bash
npm --prefix apps/codex-ui install
CODEX_UI_CWD="$PWD" npm --prefix apps/codex-ui start
```

The server listens on `127.0.0.1:4173` by default. Set `CODEX_UI_PORT` to use a
different private port.
