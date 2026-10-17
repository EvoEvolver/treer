import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
INCLUDE = re.compile(r'include_(?:str|bytes)!\(\s*"([^"]+)"')


def release_includes():
    """Files outside crates/ that release code embeds at compile time."""
    found = set()
    for source in (ROOT / "crates").rglob("*.rs"):
        if source.name.endswith("_tests.rs") or source.name == "tests.rs":
            continue
        text = source.read_text()
        # Everything from the first `#[cfg(test)]` on is test-only code.
        text = text.split("#[cfg(test)]", 1)[0]
        for relative in INCLUDE.findall(text):
            path = (source.parent / relative).resolve()
            if ROOT / "crates" not in path.parents and ROOT in path.parents:
                found.add(path.relative_to(ROOT).as_posix())
    return found


def copied(dockerfile):
    """COPY sources (from the build context) of the stage that runs cargo build."""
    stages = [[]]
    for line in (ROOT / dockerfile).read_text().splitlines():
        if line.startswith("FROM "):
            stages.append([])
        stages[-1].append(line)
    stage = next(lines for lines in stages if any("cargo build" in line for line in lines))
    sources = []
    for line in stage:
        parts = line.split()
        if parts[:1] == ["COPY"] and not any(p.startswith("--from") for p in parts):
            sources += [p.rstrip("/") for p in parts[1:-1]]
    return sources


class DockerfileIncludesTest(unittest.TestCase):
    def test_every_compile_time_include_is_in_each_image_build_context(self):
        includes = release_includes()
        self.assertIn("launchers/agent/provider-recipes.json", includes)
        for dockerfile in ("Dockerfile", "Dockerfile.hosted"):
            sources = copied(dockerfile)
            for path in sorted(includes):
                covered = any(path == s or path.startswith(s + "/") for s in sources)
                self.assertTrue(covered, f"{dockerfile} does not COPY {path}, which a crate embeds with include_str!/include_bytes!")


if __name__ == "__main__":
    unittest.main()
