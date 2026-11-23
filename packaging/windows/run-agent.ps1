param(
  [Parameter(Mandatory = $true)][string]$BinaryPath,
  [Parameter(Mandatory = $true)][string]$ConfigPath,
  [Parameter(Mandatory = $true)][string]$LogPath,
  [Parameter(Mandatory = $true)][string]$ReadyPath,
  [Parameter(Mandatory = $true)][string]$ReadyToken
)

$ErrorActionPreference = "Stop"
Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class NanoctlJob {
  [StructLayout(LayoutKind.Sequential)]
  struct Limits {
    public long ProcessTime, JobTime;
    public uint Flags;
    public UIntPtr MinimumWorkingSet, MaximumWorkingSet;
    public uint ActiveProcesses;
    public UIntPtr Affinity;
    public uint Priority, Scheduling;
  }
  [StructLayout(LayoutKind.Sequential)]
  struct IoCounters { public ulong ReadOps, WriteOps, OtherOps, ReadBytes, WriteBytes, OtherBytes; }
  [StructLayout(LayoutKind.Sequential)]
  struct ExtendedLimits {
    public Limits Basic;
    public IoCounters Io;
    public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
  }
  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  struct StartupInfo {
    public int Size;
    public string Reserved, Desktop, Title;
    public uint X, Y, XSize, YSize, XCount, YCount, Fill, Flags;
    public short ShowWindow, ReservedSize;
    public IntPtr ReservedData, Input, Output, Error;
  }
  [StructLayout(LayoutKind.Sequential)]
  struct StartupInfoEx { public StartupInfo Info; public IntPtr Attributes; }
  [StructLayout(LayoutKind.Sequential)]
  struct ProcessInfo { public IntPtr Process, Thread; public uint Id, ThreadId; }

  [DllImport("kernel32.dll", SetLastError = true)]
  static extern IntPtr CreateJobObject(IntPtr attributes, string name);
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern bool SetInformationJobObject(IntPtr job, int type, ref ExtendedLimits limits, uint size);
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref IntPtr size);
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr attribute, IntPtr value, IntPtr size, IntPtr previous, IntPtr returned);
  [DllImport("kernel32.dll")]
  static extern void DeleteProcThreadAttributeList(IntPtr list);
  [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern bool CreateProcess(string application, StringBuilder command, IntPtr processSecurity, IntPtr threadSecurity, bool inheritHandles, uint flags, IntPtr environment, string directory, ref StartupInfoEx startup, out ProcessInfo process);
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
  [DllImport("kernel32.dll", SetLastError = true)]
  static extern bool GetExitCodeProcess(IntPtr process, out uint code);
  [DllImport("kernel32.dll")]
  static extern bool CloseHandle(IntPtr handle);

  static void Check(bool success) { if (!success) throw new Win32Exception(Marshal.GetLastWin32Error()); }

  public static int Run(string binary, string arguments) {
    IntPtr job = CreateJobObject(IntPtr.Zero, null);
    if (job == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
    IntPtr list = IntPtr.Zero, jobValue = IntPtr.Zero;
    bool initialized = false;
    ProcessInfo process = new ProcessInfo();
    try {
      var limits = new ExtendedLimits();
      limits.Basic.Flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
      Check(SetInformationJobObject(job, 9, ref limits, (uint)Marshal.SizeOf(limits)));
      IntPtr size = IntPtr.Zero;
      InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref size);
      list = Marshal.AllocHGlobal(size);
      Check(InitializeProcThreadAttributeList(list, 1, 0, ref size));
      initialized = true;
      jobValue = Marshal.AllocHGlobal(IntPtr.Size);
      Marshal.WriteIntPtr(jobValue, job);
      // Assign the job atomically at creation; there is no unowned child if the task is stopped.
      Check(UpdateProcThreadAttribute(list, 0, new IntPtr(0x2000D), jobValue, new IntPtr(IntPtr.Size), IntPtr.Zero, IntPtr.Zero));
      var startup = new StartupInfoEx();
      startup.Info.Size = Marshal.SizeOf(startup);
      startup.Attributes = list;
      Check(CreateProcess(binary, new StringBuilder("\"" + binary + "\" " + arguments),
        IntPtr.Zero, IntPtr.Zero, false, 0x08080000, IntPtr.Zero,
        System.IO.Path.GetDirectoryName(binary), ref startup, out process));
      if (WaitForSingleObject(process.Process, 0xFFFFFFFF) == 0xFFFFFFFF)
        throw new Win32Exception(Marshal.GetLastWin32Error());
      uint code;
      Check(GetExitCodeProcess(process.Process, out code));
      return unchecked((int)code);
    } finally {
      if (process.Thread != IntPtr.Zero) CloseHandle(process.Thread);
      if (process.Process != IntPtr.Zero) CloseHandle(process.Process);
      if (initialized) DeleteProcThreadAttributeList(list);
      if (list != IntPtr.Zero) Marshal.FreeHGlobal(list);
      if (jobValue != IntPtr.Zero) Marshal.FreeHGlobal(jobValue);
      CloseHandle(job);
    }
  }
}
"@

try {
  Add-Content -LiteralPath $LogPath -Value "headless runner started"
  Remove-Item -LiteralPath $ReadyPath -Force -ErrorAction SilentlyContinue
  $arguments = '--config "{0}" --log-file "{1}" --ready-file "{2}" --ready-token "{3}" run' -f `
    $ConfigPath, $LogPath, $ReadyPath, $ReadyToken
  $exitCode = [NanoctlJob]::Run($BinaryPath, $arguments)
  Add-Content -LiteralPath $LogPath -Value "headless runner child exited with code $exitCode"
  exit $exitCode
} catch {
  Add-Content -LiteralPath $LogPath -Value "headless runner error: $_"
  exit 1
}
