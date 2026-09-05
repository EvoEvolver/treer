#!/usr/bin/env python3
import json
import sys
from pathlib import Path


def main() -> None:
    lock_path = Path(sys.argv[1])
    checkout = Path(sys.argv[2])
    lock = json.loads(lock_path.read_text())
    dist = checkout / lock["dist"]
    index = dist / "index.html"
    if not index.is_file():
        raise SystemExit(f"Remote Codex UI build did not produce {index}")

    marker = "treer-remote-codex-presentation"
    source = index.read_text()
    if marker not in source:
        script = (
            f'<script id="{marker}">'
            "const q=new URLSearchParams(location.search);"
            "if(!q.has('presentation')){"
            "q.set('presentation','embedded-single-thread');"
            "q.set('explorer','1');q.set('shell','0');"
            "q.set('permissions','0');q.set('nav','0');"
            "location.replace(location.pathname+'?'+q.toString()+location.hash);"
            "}"
            "</script>"
        )
        if "</head>" not in source:
            raise SystemExit(f"Remote Codex UI index has no </head>: {index}")
        index.write_text(source.replace("</head>", f"{script}</head>", 1))
    print(dist.resolve())


if __name__ == "__main__":
    main()
