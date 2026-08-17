import { readFile, writeFile } from "node:fs/promises"

const path = new URL("../dist/index.html", import.meta.url)
const html = await readFile(path, "utf8")
await writeFile(path, html.replace(/[ \t]+$/gm, ""))
