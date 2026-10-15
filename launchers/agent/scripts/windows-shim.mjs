import { execFileSync } from "node:child_process";
import { lstatSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";

// A .cmd is not a CreateProcessW executable. Compile a small console PE using
// the OS Framework compiler, never a compiler found through PATH. No shell is
// involved when this entry forwards the caller's argument vector to Node.
export function windowsShimSource(node, entry) {
  const literal = text => JSON.stringify(text).replaceAll("\u2028", "\\u2028").replaceAll("\u2029", "\\u2029");
  return `using System;
using System.Runtime.InteropServices;
using System.Text;
class TreerAgent {
  [StructLayout(LayoutKind.Sequential)] struct BasicLimits { public long ProcessTime, JobTime; public uint Flags; public UIntPtr MinWorkingSet, MaxWorkingSet; public uint ActiveProcesses; public UIntPtr Affinity; public uint Priority, Scheduling; }
  [StructLayout(LayoutKind.Sequential)] struct IoCounters { public ulong ReadOperations, WriteOperations, OtherOperations, ReadBytes, WriteBytes, OtherBytes; }
  [StructLayout(LayoutKind.Sequential)] struct ExtendedLimits { public BasicLimits Basic; public IoCounters Io; public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct Startup { public uint Size; public string Reserved, Desktop, Title; public uint X, Y, Width, Height, XChars, YChars, Fill, Flags; public ushort Show, ReservedSize; public IntPtr ReservedData, Input, Output, Error; }
  [StructLayout(LayoutKind.Sequential)] struct ProcessInfo { public IntPtr Process, Thread; public uint ProcessId, ThreadId; }
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr CreateJobObject(IntPtr attributes, string name);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetInformationJobObject(IntPtr job, int type, ref ExtendedLimits limits, uint size);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool CreateProcess(string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags, IntPtr environment, string directory, ref Startup startup, out ProcessInfo process);
  [DllImport("kernel32.dll", SetLastError=true)] static extern uint ResumeThread(IntPtr thread);
  [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr handle, uint timeout);
  [DllImport("kernel32.dll")] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
  [DllImport("kernel32.dll")] static extern bool TerminateProcess(IntPtr process, uint code);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
  static string Quote(string value) {
    var result = new StringBuilder("\\\""); int slashes = 0;
    foreach (char c in value) {
      if (c == '\\\\') { slashes++; continue; }
      if (c == '\\"') { result.Append('\\\\', slashes * 2 + 1); result.Append(c); }
      else { result.Append('\\\\', slashes); result.Append(c); }
      slashes = 0;
    }
    result.Append('\\\\', slashes * 2); result.Append('\\"'); return result.ToString();
  }
  static int Main(string[] args) {
    IntPtr job = IntPtr.Zero; ProcessInfo child = new ProcessInfo(); bool assigned = false;
    try {
      var arguments = new StringBuilder(Quote(${literal(node)}));
      arguments.Append(' '); arguments.Append(Quote(${literal(entry)}));
      foreach (string arg in args) { arguments.Append(' '); arguments.Append(Quote(arg)); }
      job = CreateJobObject(IntPtr.Zero, null);
      var limits = new ExtendedLimits(); limits.Basic.Flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
      if (job == IntPtr.Zero || !SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf(limits))) throw new Exception();
      var startup = new Startup(); startup.Size = (uint)Marshal.SizeOf(startup);
      // CREATE_SUSPENDED: the child cannot create descendants before assignment.
      // No new console, detached flag, redirection or breakaway from Host's Job.
      if (!CreateProcess(${literal(node)}, arguments, IntPtr.Zero, IntPtr.Zero, true, 4, IntPtr.Zero, null, ref startup, out child)) throw new Exception();
      if (!AssignProcessToJobObject(job, child.Process)) throw new Exception(); assigned = true;
      Console.CancelKeyPress += delegate(object sender, ConsoleCancelEventArgs e) { e.Cancel = true; };
      if (ResumeThread(child.Thread) == 0xffffffff) throw new Exception();
      if (WaitForSingleObject(child.Process, 0xffffffff) != 0) throw new Exception();
      uint code; if (!GetExitCodeProcess(child.Process, out code)) throw new Exception();
      return unchecked((int)code);
    } catch { Console.Error.WriteLine("treer-agent: selected Node or launcher could not start"); return 1; }
    finally {
      if (!assigned && child.Process != IntPtr.Zero) TerminateProcess(child.Process, 1);
      if (child.Thread != IntPtr.Zero) CloseHandle(child.Thread);
      if (child.Process != IntPtr.Zero) CloseHandle(child.Process);
      if (job != IntPtr.Zero) CloseHandle(job);
    }
  }
}
`;
}
export function buildWindowsShim(node, entry, directory) {
  const systemRoot = process.env.SystemRoot || process.env.SYSTEMROOT;
  if (!isAbsolute(systemRoot || "")) throw new Error("SystemRoot is required to build the Windows launcher entry");
  const compiler = ["Framework64", "Framework"].map(name => join(systemRoot, "Microsoft.NET", name, "v4.0.30319", "csc.exe"))
    .find(path => { try { const info = lstatSync(path); return info.isFile() && !info.isSymbolicLink(); } catch { return false; } });
  if (!compiler) throw new Error("Windows .NET Framework 4 compiler is required to install treer-agent.exe; enable the Windows .NET Framework feature");
  const temporary = mkdtempSync(join(directory, ".treer-agent-build-"));
  try {
    const source = join(temporary, "entry.cs"), output = join(temporary, "entry.exe");
    writeFileSync(source, windowsShimSource(node, entry), { flag: "wx", mode: 0o600 });
    try { execFileSync(compiler, ["/nologo", "/target:exe", "/optimize+", "/out:" + output, source], { stdio: "pipe", windowsHide: true, timeout: 25000 }); }
    catch { throw new Error("Windows launcher entry compilation failed; installation was not activated"); }
    return readFileSync(output);
  } finally { rmSync(temporary, { recursive: true, force: true }); }
}
