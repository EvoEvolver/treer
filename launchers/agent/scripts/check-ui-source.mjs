// Static security checks on ui/src (an AST walk with the TypeScript compiler API, not a text search):
//  - dangerouslySetInnerHTML / innerHTML / outerHTML / insertAdjacentHTML only in components/Markdown.tsx
//  - no eval, new Function, document.write, string-argument timers
//  - href/src/action attributes driven by data must go through safeUrl() or safeImageUrl()
//  - network calls use relative paths (the page runs behind Treer's iframe tunnel)
import ts from "typescript";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const SINK_FILE = "components/Markdown.tsx";
const URL_ATTRIBUTES = new Set(["href", "src", "action", "formAction", "poster", "data"]);
const SAFE_WRAPPERS = new Set(["safeUrl", "safeImageUrl"]);
const NETWORK_CALLS = new Set(["fetch", "request", "WebSocket", "EventSource"]);

function* files(directory) {
  for (const name of readdirSync(directory).sort()) {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else if (/\.(ts|tsx)$/.test(name)) yield path;
  }
}

export function scanSource(sourceRoot) {
  const problems = [];
  for (const path of files(sourceRoot)) {
    const rel = relative(sourceRoot, path).split("\\").join("/");
    const sink = rel === SINK_FILE;
    const text = readFileSync(path, "utf8");
    const source = ts.createSourceFile(path, text, ts.ScriptTarget.ES2022, true, path.endsWith("x") ? ts.ScriptKind.TSX : ts.ScriptKind.TS);
    const report = (node, message) => { const { line } = source.getLineAndCharacterOfPosition(node.getStart()); problems.push(`${rel}:${line + 1} ${message}`); };
    // Variables initialised straight from a safe wrapper (`const safe = safeImageUrl(x)`) may feed href/src.
    const safeNames = new Set();
    const collect = (node) => {
      if (ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.initializer && ts.isCallExpression(node.initializer) && ts.isIdentifier(node.initializer.expression) && SAFE_WRAPPERS.has(node.initializer.expression.text)) safeNames.add(node.name.text);
      ts.forEachChild(node, collect);
    };
    collect(source);
    const visit = (node) => {
      if (ts.isIdentifier(node) && node.text === "dangerouslySetInnerHTML" && !sink) report(node, "dangerouslySetInnerHTML outside the Markdown sink");
      if ((ts.isPropertyAccessExpression(node)) && !sink && ["innerHTML", "outerHTML"].includes(node.name.text)) report(node, `${node.name.text} outside the Markdown sink`);
      if (ts.isCallExpression(node)) {
        const callee = node.expression;
        const name = ts.isIdentifier(callee) ? callee.text : ts.isPropertyAccessExpression(callee) ? callee.name.text : "";
        if (name === "eval") report(node, "eval is forbidden");
        if (name === "insertAdjacentHTML" && !sink) report(node, "insertAdjacentHTML is forbidden");
        if (ts.isPropertyAccessExpression(callee) && ts.isIdentifier(callee.expression) && callee.expression.text === "document" && ["write", "writeln"].includes(callee.name.text)) report(node, "document.write is forbidden");
        if ((name === "setTimeout" || name === "setInterval") && node.arguments[0] && ts.isStringLiteralLike(node.arguments[0])) report(node, "string-argument timers are forbidden");
        if (NETWORK_CALLS.has(name) && node.arguments[0] && ts.isStringLiteralLike(node.arguments[0]) && /^(\/|[a-z][a-z0-9+.-]*:)/i.test(node.arguments[0].text)) report(node, "network paths must be relative");
      }
      if (ts.isNewExpression(node) && ts.isIdentifier(node.expression)) {
        if (node.expression.text === "Function") report(node, "new Function is forbidden");
        if (NETWORK_CALLS.has(node.expression.text) && node.arguments?.[0] && ts.isStringLiteralLike(node.arguments[0]) && /^(\/|[a-z][a-z0-9+.-]*:)/i.test(node.arguments[0].text)) report(node, "network paths must be relative");
      }
      // Only DOM elements (lowercase tags) can load a URL; component props named src/href are checked where they reach the DOM.
      const owner = ts.isJsxAttribute(node) ? node.parent?.parent : null;
      const tag = owner && (ts.isJsxOpeningElement(owner) || ts.isJsxSelfClosingElement(owner)) ? owner.tagName.getText() : "";
      if (ts.isJsxAttribute(node) && URL_ATTRIBUTES.has(node.name.getText()) && /^[a-z]/.test(tag)) {
        const init = node.initializer;
        if (init && ts.isStringLiteral(init)) { if (/^\s*(\/|javascript:|data:|[a-z][a-z0-9+.-]*:)/i.test(init.text) && !init.text.startsWith("#")) report(node, `${node.name.getText()} literal must be relative or a fragment`); }
        else if (init && ts.isJsxExpression(init) && init.expression) {
          const e = init.expression;
          const wrapped = (ts.isCallExpression(e) && ts.isIdentifier(e.expression) && SAFE_WRAPPERS.has(e.expression.text)) || (ts.isIdentifier(e) && safeNames.has(e.text));
          const template = ts.isTemplateExpression(e) && e.head.text.startsWith("#");
          const fragment = ts.isStringLiteralLike(e) && e.text.startsWith("#");
          if (!wrapped && !template && !fragment) report(node, `${node.name.getText()} must go through safeUrl()/safeImageUrl()`);
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(source);
  }
  return problems;
}
