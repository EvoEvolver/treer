import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const requiredFiles = [
  "AGENTS.md",
  "README.md",
  "docs/README.md",
  "docs/product.md",
  "docs/roadmap.md",
  "docs/architecture.md",
  "docs/security.md",
  "docs/quality.md",
  "docs/releases.md",
  "docs/canary.md",
  "docs/research/2026-08-18-project-review.md",
  "apps/README.md",
  "apps/mail/README.md",
  "apps/telegram/README.md",
  "deploy/README.md",
  "skills/treer/SKILL.md",
  "skills/treer-install/SKILL.md",
];
const excludedDirectories = new Set([
  ".git",
  ".references",
  "dist",
  "node_modules",
  "target",
]);
const errors = [];

for (const file of requiredFiles) {
  if (!existsSync(resolve(root, file))) {
    errors.push(`missing required documentation file: ${file}`);
  }
}

function requireText(file, text) {
  const path = resolve(root, file);
  if (!existsSync(path)) {
    return;
  }

  const content = readFileSync(path, "utf8");
  if (!content.includes(text)) {
    errors.push(`${file} must reference ${text}`);
  }
}

requireText("README.md", "docs/README.md");
requireText("README.md", "apps/mail/README.md");
requireText("README.md", "apps/telegram/README.md");
requireText("AGENTS.md", "docs/README.md");
requireText("AGENTS.md", "apps/README.md");
requireText("AGENTS.md", "skills/treer/SKILL.md");
requireText("AGENTS.md", "skills/treer-install/SKILL.md");
requireText("docs/README.md", "../apps/README.md");
requireText("docs/README.md", "../deploy/README.md");
requireText(
  "crates/treer-cli/src/main.rs",
  'include_str!("../../../skills/treer/SKILL.md")',
);
requireText(
  "crates/treer-protocol/src/lib.rs",
  'include_str!("../../../skills/treer-install/SKILL.md")',
);

function markdownFiles(directory) {
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && excludedDirectories.has(entry.name)) {
      continue;
    }

    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...markdownFiles(path));
    } else if (entry.isFile() && entry.name.endsWith(".md")) {
      files.push(path);
    }
  }
  return files;
}

function lineNumber(content, offset) {
  return content.slice(0, offset).split("\n").length;
}

const markdownLink = /!?\[[^\]]*\]\(([^)]+)\)/g;
for (const file of markdownFiles(root)) {
  const content = readFileSync(file, "utf8");
  for (const match of content.matchAll(markdownLink)) {
    let target = match[1].trim();
    if (target.startsWith("<")) {
      const closingBracket = target.indexOf(">");
      if (closingBracket !== -1) {
        target = target.slice(1, closingBracket);
      }
    } else {
      target = target.split(/\s+(?=["'])/, 1)[0];
    }
    if (
      target === "" ||
      target.startsWith("#") ||
      target.startsWith("//") ||
      /^[a-z][a-z\d+.-]*:/i.test(target)
    ) {
      continue;
    }

    target = target.split("#", 1)[0].split("?", 1)[0];
    try {
      target = decodeURIComponent(target);
    } catch {
      errors.push(
        `${relative(root, file)}:${lineNumber(content, match.index)} has an invalid URI: ${target}`,
      );
      continue;
    }

    const destination = resolve(dirname(file), target);
    if (destination !== root && !destination.startsWith(`${root}${sep}`)) {
      errors.push(
        `${relative(root, file)}:${lineNumber(content, match.index)} links outside the repository: ${match[1]}`,
      );
    } else if (!existsSync(destination)) {
      errors.push(
        `${relative(root, file)}:${lineNumber(content, match.index)} has a broken link: ${match[1]}`,
      );
    } else if (!statSync(destination).isFile() && !statSync(destination).isDirectory()) {
      errors.push(
        `${relative(root, file)}:${lineNumber(content, match.index)} links to an unsupported target: ${match[1]}`,
      );
    }
  }
}

if (errors.length > 0) {
  console.error("Documentation check failed:\n");
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exitCode = 1;
} else {
  console.log("Documentation check passed.");
}
