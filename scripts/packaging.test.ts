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
  doctor|unenroll) exit 0 ;;
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
