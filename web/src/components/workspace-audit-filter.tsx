import { Field } from "@/components/control-plane-ui"
import { Input } from "@/components/ui/input"
import { Select, SelectContent, SelectItem, SelectTrigger } from "@/components/ui/select"
import { t } from "@/i18n"

export interface AuditFilterOption { value: string; label: string }
export type AuditFilterKey = "actor_id" | "action" | "resource_kind"

// Suggestions are deliberately bounded; exact input always remains available.
export function auditFilterOptions(options: AuditFilterOption[]): AuditFilterOption[] {
  return [...new Map(options.filter(({ value }) => /^[A-Za-z0-9_.-]{1,128}$/.test(value))
    .map((option) => [option.value, option])).values()]
    .sort((a, b) => a.label.localeCompare(b.label) || a.value.localeCompare(b.value)).slice(0, 200)
}

export function WorkspaceAuditFilter({ field, value, options, onChange }: {
  field: AuditFilterKey; value: string; options: AuditFilterOption[]; onChange: (value: string) => void
}) {
  const label = t(`waudit.${field}`)
  const selected = options.find((option) => option.value === value)
  return <Field label={label}>
    <div className="flex min-w-0 gap-1.5">
      <Input aria-label={label} placeholder={t("waudit.exact_or_choose")} className="min-h-11 min-w-0 flex-1" maxLength={128} value={value} onChange={(event) => onChange(event.target.value)} />
      <Select value={!value ? "@all" : selected?.value ?? ""} onValueChange={(next) => onChange(next === "@all" ? "" : next)}>
        <SelectTrigger aria-label={t("waudit.choose", { filter: label })} title={t("waudit.choose", { filter: label })} className="h-11 w-11 shrink-0 justify-center px-0" />
        <SelectContent className="max-w-[calc(100vw-2rem)]">
          <SelectItem value="@all" className="min-h-11">{t("waudit.any")}</SelectItem>
          {options.map((option) => <SelectItem key={option.value} value={option.value} textValue={option.label} className="min-h-11">
            <span className="block max-w-[min(24rem,calc(100vw-6rem))] truncate" title={option.label}>{option.label}</span>
            {option.label !== option.value && <span className="block max-w-[min(24rem,calc(100vw-6rem))] truncate font-mono text-xs text-muted-foreground">{option.value}</span>}
          </SelectItem>)}
        </SelectContent>
      </Select>
    </div>
    {selected && selected.label !== value && <span className="block break-words text-xs text-muted-foreground [overflow-wrap:anywhere]">{selected.label}</span>}
  </Field>
}
