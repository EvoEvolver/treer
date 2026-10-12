"""Never let test servers inherit the invoking Agent's identity or proxy."""
import os


def hermetic_env(extra=None):
    return {**{k: v for k, v in os.environ.items()
               if not k.startswith(('TREER_', 'AGENT_UI_', 'CODEX_UI_', 'ISSUES_'))
               and k.lower() not in ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')
               and k != 'CODEX_BIN'}, **(extra or {})}
