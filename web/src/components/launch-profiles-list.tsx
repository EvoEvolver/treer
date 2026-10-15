import { EmptyState, IconButton } from "@/components/control-plane-ui"
import { type AgentLaunchProfile } from "@/lib/api"
import { formatCommandLine } from "@/lib/command-line"
import {
  Pencil,
  Play,
  Rocket,
  Trash2
} from "lucide-react"
import { t } from "@/i18n"

export function LaunchProfilesList({ profiles, loading, onEdit, onLaunch, onDelete }: { profiles: AgentLaunchProfile[]; loading: boolean; onEdit: (profile: AgentLaunchProfile) => void; onLaunch: (profile: AgentLaunchProfile) => void; onDelete: (profile: AgentLaunchProfile) => void }) {
  return <div className="border-y">
      <div className="hidden h-9 grid-cols-[minmax(0,.8fr)_minmax(0,1.6fr)_minmax(0,.7fr)_auto] items-center gap-4 border-b text-[10px] font-medium uppercase text-muted-foreground sm:grid"><span>{t("profiles.profile")}</span><span>{t("apps.command")}</span><span>{t("apps.working_directory")}</span><span className="w-28" /></div>
      {profiles.map((profile) => <div key={profile.profile_id} className="grid min-h-20 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 border-b py-3 last:border-b-0 sm:grid-cols-[minmax(0,.8fr)_minmax(0,1.6fr)_minmax(0,.7fr)_auto] sm:gap-4">
        <span className="col-start-1 row-start-1 min-w-0 sm:col-start-auto sm:row-start-auto"><span className="block truncate text-xs font-medium">{profile.name}</span>{profile.description && <span className="mt-1 block truncate text-[10px] text-muted-foreground">{profile.description}</span>}</span>
        <code className="col-start-1 row-start-2 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto" title={formatCommandLine(profile.command, profile.args)}>{formatCommandLine(profile.command, profile.args)}</code>
        <code className="col-start-1 row-start-3 min-w-0 truncate text-[10px] text-muted-foreground sm:col-start-auto sm:row-start-auto">{profile.cwd || "."}</code>
        <span className="col-start-2 row-span-3 row-start-1 flex items-center justify-end gap-1 sm:col-start-auto sm:row-span-1 sm:row-start-auto"><IconButton label={t("profiles.run", { name: profile.name })} onClick={() => onLaunch(profile)}><Play /></IconButton><IconButton label={t("profiles.edit", { name: profile.name })} onClick={() => onEdit(profile)}><Pencil /></IconButton><IconButton label={t("profiles.delete", { name: profile.name })} className="text-destructive hover:text-destructive" onClick={() => onDelete(profile)}><Trash2 /></IconButton></span>
      </div>)}
      {!profiles.length && <EmptyState icon={<Rocket />} label={loading ? t("profiles.loading_launch_profiles") : t("profiles.no_launch_profiles_yet")} />}
    </div>
}
