import type { IDisposable, Terminal } from "@xterm/xterm"

// Audited against xterm 5.5.0 InputHandler.ts and browser/Terminal.ts.
// Keep the query inventory and browser regressions in sync on xterm upgrades.
export function suppressReplayQueries(terminal: Terminal): IDisposable[] {
  return [
    terminal.parser.registerCsiHandler({ final: "c" }, () => true),
    terminal.parser.registerCsiHandler({ prefix: ">", final: "c" }, () => true),
    terminal.parser.registerCsiHandler({ final: "n" }, (params) => params[0] === 5 || params[0] === 6),
    terminal.parser.registerCsiHandler({ prefix: "?", final: "n" }, (params) => [6, 15, 25, 26, 53].includes(params[0] as number)),
    terminal.parser.registerCsiHandler({ intermediates: "$", final: "p" }, () => true),
    terminal.parser.registerCsiHandler({ prefix: "?", intermediates: "$", final: "p" }, () => true),
    terminal.parser.registerDcsHandler({ intermediates: "$", final: "q" }, () => true),
    // Window reports are option-gated (off in the console), but protect them
    // as well so future option changes cannot reintroduce historical replies.
    terminal.parser.registerCsiHandler({ final: "t" }, (params) => [14, 16, 18].includes(params[0] as number)),
    ...[4, 10, 11, 12].map((id) => terminal.parser.registerOscHandler(id, (data) => data.split(";").includes("?"))),
  ]
}
