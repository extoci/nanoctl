import { afterEach, describe, expect, test } from "bun:test";
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const temporaryDirectories: string[] = [];
const repository = resolve(import.meta.dir, "..");

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map(async (directory) => {
      await rm(directory, { recursive: true, force: true });
    }),
  );
});

async function fixture() {
  const directory = await mkdtemp(join(tmpdir(), "nanoctl-package-test-"));
  temporaryDirectories.push(directory);
  const home = join(directory, "home");
  const tools = join(directory, "tools");
  await Promise.all([mkdir(home, { recursive: true }), mkdir(tools, { recursive: true })]);
  const source = join(directory, "nanoctl");
  await writeFile(
    source,
    `#!/bin/sh
case "\${1:-}" in
  --version|doctor|unenroll) exit 0 ;;
  paths) printf 'config=%s\\n' "$HOME/.config/nanoctl/config.toml" ;;
  enroll)
    mkdir -p "$HOME/.config/nanoctl"
    printf 'device_id = "test"\\n' >"$HOME/.config/nanoctl/config.toml"
    ;;
  *) exit 0 ;;
esac
`,
  );
  await chmod(source, 0o755);
  const systemctl = join(tools, "systemctl");
  await writeFile(systemctl, "#!/bin/sh\nexit 0\n");
  await chmod(systemctl, 0o755);
  return {
    home,
    source,
    environment: {
      ...process.env,
      HOME: home,
      PATH: `${tools}:${process.env.PATH ?? ""}`,
    },
  };
}

describe("Linux user-service packaging", () => {
  test("installs, refuses overwrite, and uninstalls in the enrolled user home", async () => {
    const { home, source, environment } = await fixture();
    const install = Bun.spawn(["sh", "packaging/linux/install-user-service.sh", source], {
      cwd: repository,
      env: environment,
      stdout: "pipe",
      stderr: "pipe",
    });
    expect(await install.exited).toBe(0);
    expect(await Bun.file(join(home, ".local/bin/nanoctl")).exists()).toBe(true);
    expect(await Bun.file(join(home, ".config/systemd/user/nanoctl.service")).exists()).toBe(true);

    const duplicate = Bun.spawn(["sh", "packaging/linux/install-user-service.sh", source], {
      cwd: repository,
      env: environment,
      stdout: "pipe",
      stderr: "pipe",
    });
    expect(await duplicate.exited).not.toBe(0);

    const uninstall = Bun.spawn(["sh", "packaging/linux/uninstall-user-service.sh"], {
      cwd: repository,
      env: environment,
      stdout: "pipe",
      stderr: "pipe",
    });
    expect(await uninstall.exited).toBe(0);
    expect(await Bun.file(join(home, ".local/bin/nanoctl")).exists()).toBe(false);
    expect(await Bun.file(join(home, ".config/systemd/user/nanoctl.service")).exists()).toBe(false);
  });
});

describe("one-command Linux installer", () => {
  test("installs, enrolls, registers the user service, and updates in place", async () => {
    const { home, source, environment } = await fixture();
    const installerEnvironment = {
      ...environment,
      NANOCTL_BINARY: source,
      NANOCTL_ENROLL_CODE: "ABCDE-FGHJK-MNPQR-STVWX",
    };

    for (let run = 0; run < 2; run++) {
      const install = Bun.spawn(["sh", "install.sh"], {
        cwd: repository,
        env: installerEnvironment,
        stdout: "pipe",
        stderr: "pipe",
      });
      expect(await install.exited).toBe(0);
    }

    expect(await Bun.file(join(home, ".local/bin/nanoctl")).exists()).toBe(true);
    expect(await Bun.file(join(home, ".config/nanoctl/config.toml")).exists()).toBe(true);
    expect(await Bun.file(join(home, ".config/systemd/user/nanoctl.service")).exists()).toBe(true);
  });
});

describe("one-command Windows installer", () => {
  test("uses a headless, restartable task with a stability check", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer).toContain("-RestartInterval (New-TimeSpan -Minutes 1)");
    expect(installer).not.toContain("-RestartInterval (New-TimeSpan -Seconds");
    expect(installer).toContain("System32\\wscript.exe");
    expect(installer).toContain("run-agent.vbs");
    expect(installer).toContain("agent.ready");
    expect(installer).toContain("[IO.FileShare]::None");
    expect(installer).toContain("-Hidden");
    expect(installer).toContain("startup stability window");
    expect(installer).toContain("nanoctl.{0}.previous.exe");
    expect(installer).toContain("Migrated the existing configuration");
    expect(installer).not.toContain('New-ScheduledTaskAction -Execute $binaryPath -Argument "run"');
  });

  test("uses a direct hidden runner with file-backed diagnostics", async () => {
    const scripts = await Promise.all(
      [
        "install.ps1",
        "packaging/windows/install-service.ps1",
        "packaging/windows/update-agent.ps1",
      ].map((path) => Bun.file(join(repository, path)).text()),
    );

    for (const script of scripts) {
      expect(script).toContain('" --log-file "');
      expect(script).toContain("--ready-token");
      expect(script).toContain("Err.Clear");
      expect(script).toContain("headless runner started");
      expect(script).not.toContain("headless runner started for");
      expect(script).toContain("child exited with code");
      expect(script).toContain("shell.Run(command, 0, True)");
      expect(script).not.toContain("cmd.exe /d /s /c");
      expect(script).toContain("New-Item -ItemType File -Path $logPath -Force");
      expect(script).toContain("WorkingDirectory");
    }
  });

  test("writes the Windows Script Host runner as ASCII", async () => {
    for (const path of [
      "install.ps1",
      "packaging/windows/install-service.ps1",
      "packaging/windows/update-agent.ps1",
    ]) {
      const script = await Bun.file(join(repository, path)).text();
      const encoding = script.match(
        /Set-Content -LiteralPath \$Path -Value \$runner -Encoding (\w+) -Force/,
      )?.[1];
      expect(encoding).toBe("ASCII");
    }
  });

  test("migrates a legacy task's implicit config path and allows slow startup", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer).toContain("function Get-BinaryConfigPath");
    expect(installer).toContain("function Test-ConfigEnrolled");
    expect(installer).toContain("Get-BinaryConfigPath -Path $legacyBinaryPath");
    expect(installer).toContain("-log-file $probeLogPath --version");
    expect(installer).toContain("AddSeconds(90)");
    expect(installer).toContain("function Get-ReadyAgentProcess");
    expect(installer).toContain("-ReadyPath $ReadyPath");
    expect(installer).toContain("-BinaryPath $BinaryPath");
    expect(installer).toContain("-ReadyToken $ReadyToken");
    expect(installer).toContain("-ReadyVersion $ReadyVersion");
    expect(installer).toContain("$transactionId");
    expect(installer).toContain("$logPath");
    expect(installer).toContain("LastTaskResult");
    expect(installer).toContain("Task.Principal.LogonType");
    expect(installer).toContain("Set-OwnerProtectedAcl");
    expect(installer).toContain("icacls.exe");
    expect(installer).toContain("failed its health check");
  });

  test("accepts a legacy Administrators-owned config only for an administrator token", async () => {
    const scripts = await Promise.all(
      [
        "install.ps1",
        "packaging/windows/install-service.ps1",
        "packaging/windows/update-agent.ps1",
      ].map((path) => Bun.file(join(repository, path)).text()),
    );

    for (const script of scripts) {
      expect(script).toContain("S-1-5-32-544");
      expect(script).toContain("WindowsBuiltInRole]::Administrator");
    }
  });

  test("self-elevates the same administrator account for a legacy config repair", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer).toContain("function Test-CurrentProcessElevated");
    expect(installer).toContain("function Test-CurrentAccountAdministrator");
    expect(installer).toContain("function Invoke-ElevatedInstaller");
    expect(installer).toContain("-Verb RunAs");
    expect(installer).toContain("NANOCTL_ELEVATION_ATTEMPTED");
    expect(installer).toContain('$scriptUri = "$baseUrl/install.ps1"');
    expect(installer).toContain("Repairing the legacy administrator-owned enrollment");
    expect(installer).toContain("function Set-CurrentProcessNanoctlPath");
    expect(installer).toContain("The elevated nanoctl repair completed");
    expect(installer).toMatch(
      /Invoke-ElevatedInstaller\s+\$repairedVersion[\s\S]+Set-CurrentProcessNanoctlPath/,
    );
  });

  test("does not pass unsupported quiet switches to icacls", async () => {
    const scripts = await Promise.all(
      [
        "install.ps1",
        "packaging/windows/install-service.ps1",
        "packaging/windows/update-agent.ps1",
      ].map((path) => Bun.file(join(repository, path)).text()),
    );

    for (const script of scripts) {
      expect(script).not.toContain("/quiet");
    }
  });

  test("pins latest downloads to the resolved release and makes the installed binary win PATH", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer).toContain("Invoke-RestMethod");
    expect(installer).toContain("tag_name");
    expect(installer).toContain("$resolvedVersion");
    expect(installer).toContain("Downloading nanoctl $displayVersion for $target...");
    expect(installer).toContain("$downloadVersion");
    expect(installer).toContain("does not match the requested release");
    expect(installer).toContain("StringComparison]::OrdinalIgnoreCase");
    expect(installer).toContain('SetEnvironmentVariable("Path", $newPath, "User")');
    expect(installer).toContain("Get-Command nanoctl -All");
    expect(installer).toContain("resolvedCommandMatchesInstall");
    expect(installer).toContain("resolvedCommandVersion");
  });

  test("keeps the low-level Windows installer headless too", async () => {
    const installer = await Bun.file(
      join(repository, "packaging/windows/install-service.ps1"),
    ).text();
    expect(installer).toContain("System32\\wscript.exe");
    expect(installer).toContain("run-agent.vbs");
    expect(installer).not.toContain("$powershell");
    expect(installer).toContain("-Hidden");
    expect(installer).toContain("agent.log");
    expect(installer).toContain("-ReadyVersion $ReadyVersion");
    expect(installer).toContain("$installedVersion");
    expect(installer).toContain("$process.Path");
  });

  test("makes signed updates retryable and health-gated", async () => {
    const updater = await Bun.file(join(repository, "packaging/windows/update-agent.ps1")).text();
    expect(updater).toContain("[IO.FileShare]::None");
    expect(updater).toContain("$lockAcquired");
    expect(updater).toContain("-not $completed -and $lockAcquired");
    expect(updater).toContain("$configOwnerSid.Value -ne $currentIdentity.User.Value");
    expect(updater).toContain("function Assert-TaskOwner");
    expect(updater).toContain("Task.Principal.LogonType");
    expect(updater).toContain("Export-ScheduledTask");
    expect(updater).toContain("function Restore-PreviousTask");
    expect(updater).toContain("$previousTaskXml");
    expect(updater).toContain("Wait-AgentProcessExit");
    expect(updater).toContain("AddSeconds(90)");
    expect(updater).toContain(
      "Wait-AgentReady -BinaryPath $resolvedBinary -ReadyVersion $candidateVersion",
    );
    expect(updater).toContain("$candidateVersion");
    expect(updater).toContain("startup stability window");
    expect(updater).toContain("Set-HeadlessTaskAction");
    expect(updater).toContain("$transactionId");
    expect(updater).not.toContain("A prior update transaction must be resolved");
  });

  test("uninstalls the public per-user layout without requiring elevation", async () => {
    const uninstaller = await Bun.file(
      join(repository, "packaging/windows/uninstall-agent.ps1"),
    ).text();
    expect(uninstaller).not.toContain("#Requires -RunAsAdministrator");
    expect(uninstaller).toContain("$env:LOCALAPPDATA");
    expect(uninstaller).toContain("-Recurse -Force");
    expect(uninstaller).toContain("BinaryPath must be the standard nanoctl.exe");
    expect(uninstaller).toContain("removing the broken task");
    expect(uninstaller).toContain("Program Files installation requires an administrator");
  });
});
