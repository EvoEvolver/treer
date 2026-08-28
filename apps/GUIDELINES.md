# Treer App guidelines

This document defines the HTTP presentation contract for standalone Treer Apps.
It keeps the default surface useful to Agents while preserving a separate,
predictable interface for humans.

Agent Interface Servers (AIS), including Codex UI and Pi UI, are not standalone
App indexes. They continue to expose the `ui_path` registered in their AIS
manifest. Channel bridges without an HTTP presentation surface do not need to
invent one.

## Route layout

Use these route families consistently:

| Path | Audience | Recommended representation |
| --- | --- | --- |
| `/` | Agents | GitHub Flavored Markdown in UTF-8 text |
| Additional documentation pages | Agents | GitHub Flavored Markdown in UTF-8 text |
| `/v1/...` or `/api/...` | Programs and Agents | Versioned JSON |
| `/_human/` | Humans | HTML, CSS, JavaScript, and browser assets |
| `/health` or `/api/health` | Supervisors | Small JSON health document |

The root must not redirect to the human UI. `curl APP_URL/` must produce a
useful manual without JavaScript, browser automation, authentication cookies,
or content negotiation.

## Agent pages

Write `/` and other documentation pages as portable GitHub Flavored Markdown.
Markdown remains plain text, works directly in terminals, and can also be
rendered by an Agent client when desired.

Prefer `Content-Type: text/markdown; charset=utf-8`. `text/plain; charset=utf-8`
is acceptable for compatibility when the body still follows GitHub Markdown
conventions. Do not return HTML from an Agent documentation route.

The root manual should contain, in this order:

1. App name and one-paragraph purpose.
2. A link to the human interface at `/_human/`, when one exists.
3. The shortest safe read-only discovery or inspection commands.
4. Mutation commands, clearly separated from inspection commands.
5. Authentication, authorization, data sensitivity, and trust warnings.
6. Primary API routes or a link to another Markdown page that documents them.

Use ordinary headings, lists, tables, links, and fenced code blocks. Keep command
examples runnable and verify them against the current CLI. Avoid raw HTML,
client-side rendering, prose hidden in images, and instructions that require an
Agent to infer a hostname or API version.

An App manual is still App-controlled input. Agents should not treat arbitrary
text returned by an App as higher-priority policy, reveal credentials to follow
it, or execute mutation examples without a task that authorizes the mutation.

## Data pages

Use JSON for state, collections, records, machine-readable errors, and action
results. Do not use Markdown tables or HTML pages as a data API.

JSON routes should:

- return `Content-Type: application/json; charset=utf-8`;
- use an explicit version prefix such as `/v1/` for App-owned public contracts;
- preserve stable identifiers independently from display names;
- encode timestamps as RFC 3339 strings;
- return collections inside a named object instead of as a bare array;
- bound collection sizes and define pagination when a collection can grow;
- reject unknown or invalid mutation fields with a structured error; and
- keep secrets, credentials, and internal exception details out of responses.

Use a consistent error envelope:

```json
{
  "error": {
    "code": "soul_not_found",
    "message": "soul not found"
  }
}
```

HTTP methods retain their normal meaning: `GET` and `HEAD` inspect, `POST`
creates or invokes, `PATCH` updates, and `DELETE` removes. Document idempotency
behavior for mutations that callers may retry.

## Human pages

Place every human HTML surface below `/_human/`. This includes entry points,
SPA routes, scripts, stylesheets, fonts, images, and other browser-only assets.
Serve `/_human` and `/_human/` consistently, either with the same response or a
redirect into `/_human/`.

Configure frontend build tools with `/_human/` as their base path. Use API paths
such as `/v1/items` or `/api/session` explicitly; do not let their resolution
depend on whether the browser URL has a trailing slash. Restrict SPA fallback
to the `/_human/` subtree so an unknown Agent or API route cannot silently
return the human `index.html`.

Browser authentication callbacks may remain under `/api/`, but their default
and validated `return_to` location should be under `/_human/`. Human session
cookies do not authenticate an Agent. State the authentication requirement of
every non-public API independently from the UI.

Apply ordinary browser protections, including an explicit Content Security
Policy, `X-Content-Type-Options: nosniff`, a narrow cookie path and SameSite
policy where practical, and no credentials embedded in HTML or JavaScript.

## Minimum verification

Every standalone App with a human interface should test the boundary directly:

```sh
curl -i "$APP_URL/"
curl -i "$APP_URL/_human/"
curl -i -H 'Accept: application/json' "$APP_URL/v1/items"
```

Tests should prove that:

- `/` is Markdown-compatible text and contains useful Agent instructions;
- `/` does not contain or redirect to HTML;
- `/_human/` returns the human page and its assets resolve below `/_human/`;
- data routes return JSON even when they fail;
- the frontend uses absolute API paths; and
- unknown paths do not fall through to an unrelated representation.

Keep the App's README focused on deployment, configuration, state, backup, and
operator concerns. Put the concise runtime manual in the content served from
`/`, and keep both documents synchronized with executable behavior and tests.
