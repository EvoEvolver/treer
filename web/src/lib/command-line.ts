import { parse, quote } from "shell-quote"
import { t } from "@/i18n"

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
    throw new Error(t("cmd.command_has_invalid_quoting"))
  }

  const words = entries.filter((entry): entry is string => typeof entry === "string")
  if (!words.length || words.length !== entries.length) {
    throw new Error(t("cmd.command_must_be_one_executable"))
  }

  const [command, ...args] = words
  if (!command) throw new Error(t("cmd.command_is_required"))
  return { command, args }
}
