import { afterEach, describe, expect, test } from "bun:test";
import { existsSync } from "node:fs";
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const temporaryDirectories: string[] = [];
const repository = resolve(import.meta.dir, "..");
const describeUnix = describe.skipIf(process.platform === "win32");
const testUnix = test.skipIf(process.platform === "win32");

afterEach(async () => {
  await Promise.all(
    temporaryDirectories.splice(0).map(async (directory) => {
      await rm(directory, { recursive: true, force: true });
    }),
  );
});

async function fixture(platform = "Linux", machine = platform === "Darwin" ? "arm64" : "x86_64") {
  const directory = await mkdtemp(join(tmpdir(), "nanoctl-package-test-"));
  temporaryDirectories.push(directory);
  const home = join(directory, "home");
  const tools = join(directory, "tools");
  await Promise.all([
    mkdir(join(home, ".config/nanoctl"), { recursive: true }),
    mkdir(tools, { recursive: true }),
  ]);
  await writeFile(join(home, ".config/nanoctl/config.toml"), 'device_id = "test"\n');
  const source = join(directory, "nanoctl");
  await writeFile(
    source,
    `#!/bin/sh
case "\${1:-}" in
  --version) printf 'nanoctl 1.0.0\\n' ;;
  config) exit 0 ;;
  paths) printf 'config=%s\\n' "$HOME/.config/nanoctl/config.toml" ;;
  url)
    [ "\${NANOCTL_TEST_URL_FAIL:-}" != 1 ] || exit 1
    printf 'https://test.tailnet.ts.net:7337\\n' ;;
  *) exit 42 ;;
esac
`,
  );
  await chmod(source, 0o755);
  async function command(name: string, script: string) {
    const path = join(tools, name);
    await writeFile(path, `#!/bin/sh\n${script}\n`);
    await chmod(path, 0o755);
  }

  await Promise.all([
    command("tailscale", `printf '{"BackendState":"%s"}\\n' "\${NANOCTL_TEST_TAILNET:-Running}"`),
    command(
      "security",
      `printf '%s\\n' "$*" >>"$HOME/credential-cleanup"; exit "\${NANOCTL_TEST_CLEANUP_EXIT:-0}"`,
    ),
    command(
      "keyctl",
      `printf '%s\\n' "$*" >>"$HOME/credential-cleanup"
[ "\${NANOCTL_TEST_CLEANUP_EXIT:-0}" = 0 ] || exit 1
case "$1" in get_persistent) echo 77 ;; search) echo 42 ;; esac`,
    ),
    command("uname", `case "$1" in -s) echo ${platform} ;; -m) echo ${machine} ;; esac`),
    command(
      "systemctl",
      `printf '%s\\n' "$*" >>"$HOME/service-calls"
case "$2" in
  is-active)
    if [ -f "$HOME/running" ]; then echo active; else echo inactive; exit 3; fi ;;
  show)
    if [ -f "$HOME/running" ]; then echo 123; else echo 0; fi ;;
  stop) rm -f "$HOME/running" ;;
  start) "$HOME/.local/bin/nanoctl" --version >"$HOME/running" ;;
  enable)
    if [ "$3" = --now ]; then "$HOME/.local/bin/nanoctl" --version >"$HOME/running"; fi ;;
esac`,
    ),
    command(
      "launchctl",
      `printf '%s\\n' "$*" >>"$HOME/service-calls"
case "$1" in
  print)
    case "$2" in
      */dev.nanoctl.agent)
        [ -f "$HOME/registered" ] || exit 1
        if [ -f "$HOME/running" ]; then printf '\\tpid = 123\\n'; fi ;;
    esac ;;
  bootout) rm -f "$HOME/registered" "$HOME/running" ;;
  bootstrap|kickstart)
    : >"$HOME/registered"
    "$HOME/.local/bin/nanoctl" --version >"$HOME/running" ;;
esac`,
    ),
    command(
      "sleep",
      `if [ "\${NANOCTL_TEST_PAUSE_STARTUP:-}" = 1 ]; then
  : >"$HOME/paused-startup"
  while [ ! -f "$HOME/resume-startup" ]; do /bin/sleep 0.01; done
fi
if [ -f "$HOME/running" ] && grep -q 'nanoctl 2.0.0-broken' "$HOME/running"; then
  rm "$HOME/running"
fi`,
    ),
  ]);
  return {
    home,
    tools,
    source,
    environment: {
      ...process.env,
      HOME: home,
      PATH: `${tools}:${process.env.PATH ?? ""}`,
    },
  };
}

async function installUnix(environment: NodeJS.ProcessEnv, source: string) {
  const install = Bun.spawn(["sh", "install.sh"], {
    cwd: repository,
    env: {
      ...environment,
      NANOCTL_BINARY: source,
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  const [exit, stdout, stderr] = await Promise.all([
    install.exited,
    install.stdout.text(),
    install.stderr.text(),
  ]);
  return { exit, stdout, stderr };
}

describeUnix("Linux user-service packaging", () => {
  test("installs, refuses overwrite, and uninstalls in the desktop user home", async () => {
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

describeUnix("one-command Linux installer", () => {
  test("installs, registers the user service, and updates without changing legacy config", async () => {
    const { home, source, environment } = await fixture();
    expect((await installUnix(environment, source)).exit).toBe(0);
    const update = (await Bun.file(source).text()).replace("nanoctl 1.0.0", "nanoctl 2.0.0");
    await writeFile(source, update);
    expect((await installUnix(environment, source)).exit).toBe(0);

    expect(await Bun.file(join(home, ".local/bin/nanoctl")).exists()).toBe(true);
    expect(await Bun.file(join(home, "running")).text()).toBe("nanoctl 2.0.0\n");
    expect(await Bun.file(join(home, ".config/nanoctl/config.toml")).exists()).toBe(true);
    expect(await Bun.file(join(home, ".config/systemd/user/nanoctl.service")).exists()).toBe(true);
  });

  test("rejects a concurrent installation until the first installer finishes", async () => {
    const { home, source, environment } = await fixture();
    const paused = join(home, "paused-startup");
    const first = installUnix({ ...environment, NANOCTL_TEST_PAUSE_STARTUP: "1" }, source);
    try {
      for (let attempt = 0; attempt < 100 && !existsSync(paused); attempt++) {
        await Bun.sleep(10);
      }
      expect(existsSync(paused)).toBe(true);
      const concurrent = await installUnix(environment, source);
      expect(concurrent.exit).not.toBe(0);
      expect(concurrent.stderr).toContain("another installation may be running");
    } finally {
      await writeFile(join(home, "resume-startup"), "");
      await first;
    }
    expect((await first).exit).toBe(0);
    expect((await installUnix(environment, source)).exit).toBe(0);
  });
});

testUnix("rejects Intel macOS before installing", async () => {
  const { source, environment } = await fixture("Darwin", "x86_64");
  const install = await installUnix(environment, source);
  expect(install.exit).not.toBe(0);
  expect(install.stderr).toContain("unsupported platform: Darwin x86_64");
});

testUnix("prints the macOS URL from its stable launch path", async () => {
  const { home, source, environment } = await fixture("Darwin");
  const original = await Bun.file(source).text();
  await writeFile(
    source,
    original.replace("  url)", '  url) [ "$0" = "$HOME/.local/bin/nanoctl" ] || exit 42;'),
  );

  expect((await installUnix(environment, source)).exit).toBe(0);
  expect(await Bun.file(join(home, "running")).text()).toBe("nanoctl 1.0.0\n");
});

for (const [platform, registration] of [
  ["Linux", ".config/systemd/user/nanoctl.service"],
  ["Darwin", "Library/LaunchAgents/dev.nanoctl.agent.plist"],
] as const) {
  describeUnix(`one-command ${platform} installer rollback`, () => {
    test("rejects an invalid candidate before contacting the service manager", async () => {
      const { home, source, environment } = await fixture(platform);
      expect((await installUnix(environment, source)).exit).toBe(0);
      const original = await Bun.file(source).text();
      const calls = Bun.file(join(home, "service-calls"));
      const before = await calls.text();
      await writeFile(source, original.replace("printf 'config=%s", "printf 'invalid=%s"));

      const upgrade = await installUnix(environment, source);
      expect(upgrade.exit).not.toBe(0);
      expect(await calls.text()).toBe(before);
      expect(await Bun.file(join(home, ".local/bin/nanoctl")).text()).toBe(original);
      expect(await Bun.file(join(home, "running")).text()).toBe("nanoctl 1.0.0\n");
      expect(upgrade.stderr).toContain("invalid configuration path");
    });

    test("restores the previous binary and service when the candidate exits during startup", async () => {
      const { home, source, environment } = await fixture(platform);
      expect((await installUnix(environment, source)).exit).toBe(0);
      const original = await Bun.file(source).text();
      const servicePath = join(home, registration);
      const service = `${await Bun.file(servicePath).text()}\n# Local service customization\n`;
      await writeFile(servicePath, service);
      await writeFile(source, original.replace("nanoctl 1.0.0", "nanoctl 2.0.0-broken"));

      const upgrade = await installUnix(environment, source);
      expect(upgrade.exit).not.toBe(0);
      expect(upgrade.stderr).toContain("candidate service did not stay running");
      expect(upgrade.stdout).not.toContain("installed and running");
      expect(await Bun.file(join(home, ".local/bin/nanoctl")).text()).toBe(original);
      expect(await Bun.file(servicePath).text()).toBe(service);
      expect(await Bun.file(join(home, "running")).text()).toBe("nanoctl 1.0.0\n");
      expect(await Bun.file(join(home, ".config/nanoctl/config.toml")).text()).toBe(
        'device_id = "test"\n',
      );
    });

    test("keeps a previously stopped service stopped after rollback", async () => {
      const { home, source, environment } = await fixture(platform);
      expect((await installUnix(environment, source)).exit).toBe(0);
      const original = await Bun.file(source).text();
      await rm(join(home, "running"));
      await writeFile(source, original.replace("nanoctl 1.0.0", "nanoctl 2.0.0-broken"));

      const upgrade = await installUnix(environment, source);
      expect(upgrade.exit).not.toBe(0);
      expect(upgrade.stderr).toContain("candidate service did not stay running");
      expect(await Bun.file(join(home, ".local/bin/nanoctl")).text()).toBe(original);
      expect(await Bun.file(join(home, registration)).exists()).toBe(true);
      expect(await Bun.file(join(home, "running")).exists()).toBe(false);
    });

    test("removes the broken binary and registration on a failed first install", async () => {
      const { home, source, environment } = await fixture(platform);
      const original = await Bun.file(source).text();
      await writeFile(source, original.replace("nanoctl 1.0.0", "nanoctl 2.0.0-broken"));

      const install = await installUnix(environment, source);
      expect(install.exit).not.toBe(0);
      expect(install.stderr).toContain("candidate service did not stay running");
      expect(await Bun.file(join(home, ".local/bin/nanoctl")).exists()).toBe(false);
      expect(await Bun.file(join(home, registration)).exists()).toBe(false);
      expect(await Bun.file(join(home, "running")).exists()).toBe(false);

      await writeFile(source, original);
      expect((await installUnix(environment, source)).exit).toBe(0);
    });

    test("rejects invalid config without starting or replacing the service", async () => {
      const { home, source, environment } = await fixture(platform);
      const original = await Bun.file(source).text();
      await writeFile(source, original.replace("  config)", "  config) exit 42;"));

      const install = await installUnix(environment, source);
      expect(install.exit).toBe(1);
      expect(install.stderr).toContain("candidate configuration is invalid");
      expect(await Bun.file(join(home, ".local/bin/nanoctl")).exists()).toBe(false);
      expect(await Bun.file(join(home, registration)).exists()).toBe(false);
      expect(await Bun.file(join(home, "running")).exists()).toBe(false);
      expect(await Bun.file(join(home, "service-calls")).exists()).toBe(false);
    });

    test("installs without a config, warns for stopped Tailscale and tolerates unavailable URL", async () => {
      const { home, source, environment } = await fixture(platform);
      await rm(join(home, ".config/nanoctl"), { recursive: true });
      const install = await installUnix(
        { ...environment, NANOCTL_TEST_TAILNET: "Stopped", NANOCTL_TEST_URL_FAIL: "1" },
        source,
      );
      expect(install.exit).toBe(0);
      expect(install.stderr).toContain("https://tailscale.com/download");
      expect(install.stderr).toContain("URL unavailable yet");
      expect(install.stdout).toContain("installed and running");
    });

    test("prints the URL and removes only the legacy credential best-effort", async () => {
      const { home, source, environment } = await fixture(platform);
      const install = await installUnix(environment, source);
      expect(install.exit).toBe(0);
      expect(install.stdout.trim().endsWith("https://test.tailnet.ts.net:7337")).toBe(true);
      expect(install.stderr).not.toContain("https://tailscale.com/download");
      const cleanup = await Bun.file(join(home, "credential-cleanup")).text();
      expect(cleanup).toContain(
        platform === "Linux" ? "keyring-rs:test@dev.nanoctl.agent" : "-s dev.nanoctl.agent -a test",
      );
      expect(await Bun.file(join(home, ".config/nanoctl/config.toml")).text()).toBe(
        'device_id = "test"\n',
      );
      expect(
        (await installUnix({ ...environment, NANOCTL_TEST_CLEANUP_EXIT: "1" }, source)).exit,
      ).toBe(0);
    });

    test("missing Tailscale is nonfatal", async () => {
      const { source, tools, environment } = await fixture(platform);
      await rm(join(tools, "tailscale"));
      const install = await installUnix(environment, source);
      expect(install.exit).toBe(0);
      expect(install.stderr).toContain("https://tailscale.com/download");
    });
  });
}

describe("one-command Windows installer", () => {
  test("publishes only the x64 Windows target", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer).toContain('"AMD64" { "windows-x64" }');
    expect(installer).not.toContain('"ARM64" { "windows-arm64" }');
  });

  test.skipIf(process.platform !== "win32")(
    "checks native job lifetime and stop edge cases",
    async () => {
      const child = Bun.spawn(
        [
          "powershell.exe",
          "-NoProfile",
          "-NonInteractive",
          "-File",
          join(repository, "scripts/windows-lifecycle.test.ps1"),
        ],
        { stdout: "pipe", stderr: "pipe" },
      );
      const [stdout, stderr, code] = await Promise.all([
        new Response(child.stdout).text(),
        new Response(child.stderr).text(),
        child.exited,
      ]);
      expect(stderr).toBe("");
      expect(stdout).toContain("WINDOWS_LIFECYCLE_OK");
      expect(code).toBe(0);
    },
    30_000,
  );

  test("uses nonfatal tailnet discovery and legacy-only credential cleanup without enrollment", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    const helper = await Bun.file(join(repository, "packaging/tailnet.ps1")).text();
    expect(installer).toContain(helper.trim());
    expect(helper).toContain("https://tailscale.com/download");
    expect(helper).toContain('if ($LASTEXITCODE -ne 0 -or -not $url) { throw "URL unavailable." }');
    expect(helper).toContain('"/delete:$($Matches[1]).dev.nanoctl.agent"');
    expect(helper).toContain("& $BinaryPath --config $ConfigPath url --wait");
    expect(helper).toContain('Write-Host "  $url"');
    for (const path of [
      "install.ps1",
      "packaging/windows/install-service.ps1",
      "packaging/windows/update-agent.ps1",
      "packaging/windows/uninstall-agent.ps1",
    ]) {
      const script = await Bun.file(join(repository, path)).text();
      expect(script).not.toMatch(
        /Read-Host|\benroll\b|\bunenroll\b|Test-ConfigEnrolled|ControlPlane/,
      );
    }
  });
  test("uses a headless, restartable task with a stability check", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer).toContain("-RestartInterval (New-TimeSpan -Minutes 1)");
    expect(installer).not.toContain("-RestartInterval (New-TimeSpan -Seconds");
    expect(installer).toContain("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    expect(installer).toContain("run-agent.ps1");
    expect(installer).toContain("agent.ready");
    expect(installer).toContain("[IO.FileShare]::None");
    expect(installer).toContain("-Hidden");
    expect(installer).toContain("startup stability window");
    expect(installer).toContain("nanoctl.{0}.previous.exe");
    expect(installer).toContain("Preserving the existing configuration");
    expect(installer).toContain("Interactive|InteractiveToken");
    expect(installer).not.toContain('New-ScheduledTaskAction -Execute $binaryPath -Argument "run"');
  });

  test("owns the hidden agent with a kill-on-close job from process creation", async () => {
    const runner = await Bun.file(join(repository, "packaging/windows/run-agent.ps1")).text();
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer.replaceAll("\r\n", "\n")).toContain(runner.trim().replaceAll("\r\n", "\n"));
    expect(runner).toContain("JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE");
    expect(runner).toContain("UpdateProcThreadAttribute");
    expect(runner).toContain("new IntPtr(0x2000D)");
    expect(runner).toContain("inheritHandles");
    expect(runner).toContain("false, 0x08080000");
    expect(runner).toContain("CloseHandle(job)");
    expect(runner).toContain("--log-file");
    expect(runner).toContain("--ready-token");
    expect(runner).toContain("headless runner started");
    expect(runner).toContain("child exited with code");
    for (const path of [
      "install.ps1",
      "packaging/windows/install-service.ps1",
      "packaging/windows/update-agent.ps1",
    ]) {
      const script = await Bun.file(join(repository, path)).text();
      expect(script).toContain("-WindowStyle Hidden");
      expect(script).toContain("run-agent.ps1");
      expect(script).not.toContain("shell.Run(command, 0, True)");
      expect(script).toContain("New-Item -ItemType File -Path $logPath -Force");
      expect(script).toContain("WorkingDirectory");
    }
  });

  test("stops only matching executable paths and waits before replacing or deleting", async () => {
    const helper = await Bun.file(join(repository, "packaging/windows/lifecycle.ps1")).text();
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer.replaceAll("\r\n", "\n")).toContain(helper.trim().replaceAll("\r\n", "\n"));
    expect(helper).toContain("$normalizedPaths -contains [IO.Path]::GetFullPath($_.Path)");
    expect(helper).toContain('-ArgumentList "stop"');
    expect(helper).toContain("$unrelated.Count -eq 0");
    expect(helper).toContain("$stopCommand.WaitForExit(70000)");
    expect(helper).toContain("$process.Kill()");
    expect(helper).toContain("$process.WaitForExit(30000)");
    expect(helper).not.toContain("Stop-Process -Name");
    for (const path of [
      "packaging/windows/install-service.ps1",
      "packaging/windows/update-agent.ps1",
      "packaging/windows/uninstall-agent.ps1",
    ]) {
      const script = await Bun.file(join(repository, path)).text();
      expect(script).toContain('lifecycle.ps1"');
      expect(script).toContain("Stop-AgentProcesses -Paths");
    }
    const uninstaller = await Bun.file(
      join(repository, "packaging/windows/uninstall-agent.ps1"),
    ).text();
    expect(uninstaller.indexOf("Stop-AgentProcesses -Paths")).toBeLessThan(
      uninstaller.indexOf("Unregister-ScheduledTask"),
    );
    expect(uninstaller.lastIndexOf("Stop-AgentProcesses -Paths")).toBeLessThan(
      uninstaller.indexOf("Remove-Item -LiteralPath $installRoot"),
    );
  });

  test("migrates a legacy task's implicit config path and allows slow startup", async () => {
    const installer = await Bun.file(join(repository, "install.ps1")).text();
    expect(installer).toContain("function Get-BinaryConfigPath");
    expect(installer).toContain("function Test-ConfigFile");
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
    expect(installer).toContain("Interactive|InteractiveToken");
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
    expect(installer).toContain("Repairing the legacy administrator-owned configuration");
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
    expect(installer).toContain("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
    expect(installer).toContain("run-agent.ps1");
    expect(installer).toContain("-WindowStyle Hidden");
    expect(installer).toContain("-Hidden");
    expect(installer).toContain("agent.log");
    expect(installer).toContain("-ReadyVersion $ReadyVersion");
    expect(installer).toContain("$installedVersion");
    expect(installer).toContain("$process.Path");
    expect(installer).toContain("function Get-TaskConfigPath");
    expect(installer).toContain("function Get-TaskBinaryPaths");
    expect(installer).toContain("$previousProcessPaths");
    expect(installer).toContain("Get-TaskBinaryPaths -Task $existingTask");
    expect(installer).toContain("Preserving the existing configuration");
    expect(installer).toContain("previous-task.xml");
    expect(installer).toContain("Wait-AgentProcessExit");
    expect(installer).toContain("install.lock");
    expect(installer).toContain("reinstall rollback");
    expect(installer).not.toContain("already installed. Uninstall or update it explicitly");
    expect(installer).not.toContain("install directory already exists. Remove it explicitly");
  });

  test("makes signed updates retryable and health-gated", async () => {
    const updater = await Bun.file(join(repository, "packaging/windows/update-agent.ps1")).text();
    expect(updater).toContain("[IO.FileShare]::None");
    expect(updater).toContain("$lockAcquired");
    expect(updater).toContain("-not $completed -and $lockAcquired");
    expect(updater).toContain("$configOwnerSid.Value -ne $currentIdentity.User.Value");
    expect(updater).toContain("function Assert-TaskOwner");
    expect(updater).toContain("Task.Principal.LogonType");
    expect(updater).toContain("Interactive|InteractiveToken");
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
