import { chmodSync, closeSync, fsyncSync, lstatSync, openSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { isAbsolute, join, toNamespacedPath } from "node:path";

export function syncDirectory(directory) {
  // Node cannot open/flush directory handles on Windows. File fsync and atomic
  // rename still apply; do not claim Unix directory-fsync power-loss guarantees.
  if (process.platform === "win32") return;
  const fd = openSync(directory, "r"); try { fsyncSync(fd); } finally { closeSync(fd); }
}
export function protectDirectory(directory) {
  const info = lstatSync(directory);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error("Private state directory must not be a link");
  if (process.platform !== "win32") { chmodSync(directory, 0o700); return; }
  const env = {};
  for (const [key, value] of Object.entries(process.env)) {
    if (/^(SystemRoot|windir|USERPROFILE|USERNAME|APPDATA|LOCALAPPDATA|ProgramData|HOMEDRIVE|HOMEPATH|TEMP|TMP)$/i.test(key)) env[key] = value;
  }
  const systemRoot = process.env.SystemRoot || process.env.SYSTEMROOT || process.env.windir;
  if (!systemRoot || !isAbsolute(systemRoot)) throw new Error("SystemRoot is required to protect Windows Agent state");
  env.TREER_PRIVATE_STATE_PATH = toNamespacedPath(directory);
  const script = "$ErrorActionPreference='Stop'; $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value; $acl=New-Object Security.AccessControl.DirectorySecurity; $acl.SetSecurityDescriptorSddlForm(('D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;'+$sid+')')); [IO.Directory]::SetAccessControl($env:TREER_PRIVATE_STATE_PATH,$acl)";
  try {
    execFileSync(join(systemRoot, "System32/WindowsPowerShell/v1.0/powershell.exe"), ["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(script, "utf16le").toString("base64")], { env, stdio: "ignore", windowsHide: true, timeout: 15000 });
  } catch { throw new Error("Unable to apply current-user/SYSTEM-only ACL to Windows Agent state"); }
}
