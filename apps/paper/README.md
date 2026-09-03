# Treer Paper

Paper is a deliberately small collaborative LaTeX App for trusted Treer
workspaces. One Node process serves the browser editor and Yjs WebSocket rooms.
The canonical project, Yjs snapshots, and latest PDF are ordinary files; there
is no account system or database.

## Development

```sh
npm --prefix apps/paper install
npm --prefix apps/paper test
PAPER_STATE_DIR="$PWD/.treer/apps/paper" npm --prefix apps/paper start
```

Open `http://127.0.0.1:8090/_human/`. Set `PAPER_PORT` or `PAPER_HOST` to change
the listener. `PAPER_LATEX_BIN` may point to Tectonic or `latexmk`; otherwise
Paper checks `.treer/apps/paper/bin/tectonic`, then `PATH`.

## Agent Editing

`GET /v1/files?path=main.tex` returns the current Yjs text plus an
`X-Content-SHA256` header. Agents can submit non-overlapping UTF-16 ranges with
`POST /v1/files/patch?path=main.tex`:

```json
{
  "baseSha256": "64-character lowercase digest from the GET response",
  "agent": { "id": "ag_...", "name": "writer" },
  "changes": [
    { "from": 120, "to": 128, "insert": "replacement" }
  ]
}
```

Patch requests default to `mode: "suggesting"`. The server records replacements
as paired deletion/addition reviews authored as
`Agent: writer [ag_...]`; insertions and deletions use the corresponding single
review. An explicit `mode: "direct"` bypasses review creation. The server
validates every range before applying all edits in one Yjs transaction. A
concurrent edit returns `409 stale_file`; the Agent must fetch the current text
and recompute its patch. Offsets use JavaScript/CodeMirror UTF-16 units and
cannot split a Unicode surrogate pair. Suggesting patches that overlap an open
review return `409 review_conflict` rather than nesting storage macros.

## State And Backup

The default state root is `.treer/apps/paper` relative to the launch directory:

- `project/` is the canonical project tree.
- `yjs/` preserves CRDT identity across restarts.
- `build/latest.pdf` and `build/latest.json` hold the latest build.
- `cache/` is compiler-owned package cache.

Stop the App or copy `project/` and `yjs/` together for a consistent backup.
The plain project files remain usable without Yjs state.

## Review Syntax

Comments and revisions live in the LaTeX source rather than a database:

```tex
\cmtbg{comment-id}{Ada}selected text\cmted{Check this claim.}
\addbg{revision-id}{Lin}inserted text\added
\delbg{revision-id}{Lin}deleted text\deled
```

`Suggest` is a mode toggle. While it is active, ordinary insert, delete, and
replace operations create these addition/deletion pairs automatically; users
do not select text and fill out a revision dialog.

Compilation strips review storage from every `.tex` file in its temporary
tree. The PDF keeps commented text and suggested additions, removes suggested
deletions, and never displays comment notes, revision marks, authors, or Paper
macros. Canonical project files retain the review data. Resolving a comment
keeps its selected text. Accepting a revision keeps additions and removes
deletions; rejecting it does the inverse. The browser hides and protects these
storage macros: comments appear as highlighted text with hover cards, while
revisions show inserted and struck-through text inline.
Selecting a range that intersects one or more revisions shows a floating action
that accepts all of those revisions in one collaborative edit.

## Trust Boundary

Paper has no application authentication. Treer workspace ingress can protect
the public browser origin, but every Agent able to reach its internal hostname
can read or modify the project. Compilation is not a security sandbox. Run it
only for trusted users, expose no secrets to the process, and do not place other
workspace data beneath its project directory.
