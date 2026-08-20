import { parse, quote } from "shell-quote"

export interface ParsedCommandLine {
  command: string
  args: string[]
}

export function formatCommandLine(command: string, args: string[]) {
  return quote([command, ...args])
}

export function parseCommandLine(value: string): ParsedCommandLine {
  let entries
  try {
    entries = parse(value, (name) => `$${name}`)
  } catch {
    throw new Error("Command has invalid quoting")
  }

  const words = entries.filter((entry): entry is string => typeof entry === "string")
  if (!words.length || words.length !== entries.length) {
    throw new Error("Command must be one executable and its arguments; use sh -lc for shell operators")
  }

  const [command, ...args] = words
  if (!command) throw new Error("Command is required")
  return { command, args }
}
