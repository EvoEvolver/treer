Treer Soul
==========

This index is for Agents. Browsers and `Accept: text/html` receive the read-only
human interface from this same URL.

Soul stores immutable Agent state archives and can create a Treer Agent from a
stored archive. The default service URL is http://soul.internal. Do not use a
public ingress: every Agent that can reach this service can read every Soul.

Install the client
------------------

    curl -fsSL http://soul.internal/install.sh | sh

The installer adds treer-soul to ~/.local/bin. Set TREER_SOUL_URL when this App
uses a different hostname.

Inspect Souls
-------------

    treer-soul list
    treer-soul show soul_ID

Capture and incarnate a Codex session
-------------------------------------

    treer-soul capture-codex --name current-codex
    treer-soul incarnate soul_ID --machine self --name codex-reborn --cwd .

The target machine must have a compatible Codex CLI and its own authentication.
Do not resume one Codex session concurrently from multiple running Agents.

Generic Soul
------------

A manifest maps environment variable names to uploaded relative file paths:

    {
      "schema_version": 1,
      "name": "Example state",
      "environment": {"AGENT_TRACE_PATH": "files/trace.jsonl"}
    }

Upload and incarnate it with an explicit command:

    treer-soul upload --manifest manifest.json \
      --file files/trace.jsonl=./trace.jsonl
    treer-soul incarnate soul_ID --machine self --name reborn --cwd . -- command arg

HTTP API
--------

    GET  /health
    GET  /v1/souls
    GET  /v1/souls/soul_ID
    GET  /v1/souls/soul_ID/archive
    POST /v1/souls                         Content-Type: application/x-tar
    POST /v1/souls/soul_ID/incarnations   Content-Type: application/json

Use treer-soul instead of constructing upload or incarnation requests manually.
Soul archives can contain prompts, traces, tool output, and secrets. Inspect the
manifest and protect downloaded files accordingly.
