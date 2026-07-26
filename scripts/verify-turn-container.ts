import { chmod, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHmac, randomBytes } from "node:crypto";

const IMAGE = "nanoctl/coturn:verification";
const CONTAINER = `nanoctl-turn-verification-${process.pid}`;

async function run(
  command: string[],
  options: { quiet?: boolean; allowFailure?: boolean } = {},
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
  const process = Bun.spawn(command, {
    cwd: import.meta.dir + "/..",
    stdout: "pipe",
    stderr: "pipe",
  });
  const [exitCode, stdout, stderr] = await Promise.all([
    process.exited,
    new Response(process.stdout).text(),
    new Response(process.stderr).text(),
  ]);
  if (!options.quiet) {
    if (stdout) processOutput(stdout);
    if (stderr) processOutput(stderr);
  }
  if (exitCode !== 0 && !options.allowFailure) {
    throw new Error(`${command[0]} exited with ${exitCode}`);
  }
  return { exitCode, stdout, stderr };
}

function processOutput(value: string): void {
  process.stdout.write(value);
}

async function main(): Promise<void> {
  if (process.platform !== "linux") {
    throw new Error("TURN container verification requires Linux host networking");
  }
  const workspace = await mkdtemp(join(tmpdir(), "nanoctl-turn-"));
  const secretPath = join(workspace, "turn_auth_secret");
  const secret = randomBytes(32).toString("hex");
  await Bun.write(secretPath, `${secret}\n`, { mode: 0o400 });
  await chmod(secretPath, 0o444);

  try {
    await run(["docker", "build", "-t", IMAGE, "infra/coturn"]);
    await run([
      "docker",
      "run",
      "-d",
      "--name",
      CONTAINER,
      "--network",
      "host",
      "--read-only",
      "--tmpfs",
      "/tmp:size=1m,mode=0700,uid=65534,gid=65534",
      "--cap-drop",
      "ALL",
      "--cap-add",
      "NET_BIND_SERVICE",
      "--security-opt",
      "no-new-privileges:true",
      "-e",
      "TURN_REALM=turn.test",
      "-e",
      "TURN_EXTERNAL_IP=127.0.0.1",
      "-e",
      "TURN_LISTEN_IP=127.0.0.1",
      "-e",
      "TURN_RELAY_IP=127.0.0.1",
      "-e",
      "TURN_TLS_ENABLED=0",
      "-v",
      `${secretPath}:/run/secrets/turn_auth_secret:ro`,
      IMAGE,
      "--allow-loopback-peers",
      "--allowed-peer-ip=127.0.0.1",
    ]);

    let ready = false;
    for (let attempt = 0; attempt < 10; attempt += 1) {
      const check = await run(
        ["docker", "exec", CONTAINER, "turnutils_stunclient", "-p", "3478", "127.0.0.1"],
        { quiet: true, allowFailure: true },
      );
      if (check.exitCode === 0) {
        ready = true;
        break;
      }
      await Bun.sleep(500);
    }
    if (!ready) throw new Error("coturn did not become ready");

    const expires = Math.floor(Date.now() / 1000) + 300;
    const username = `${expires}:integration-session`;
    const password = createHmac("sha1", secret).update(username).digest("base64");
    const invalid = await run(
      [
        "docker",
        "exec",
        CONTAINER,
        "turnutils_uclient",
        "-y",
        "-c",
        "-n",
        "1",
        "-u",
        username,
        "-w",
        "invalid",
        "127.0.0.1",
      ],
      { quiet: true, allowFailure: true },
    );
    if (invalid.exitCode === 0) throw new Error("coturn accepted an invalid REST credential");

    await run([
      "docker",
      "exec",
      CONTAINER,
      "turnutils_uclient",
      "-y",
      "-c",
      "-n",
      "2",
      "-u",
      username,
      "-w",
      password,
      "127.0.0.1",
    ]);

    const inspection = await run(
      ["docker", "inspect", "--format", "{{json .Config.Env}}", CONTAINER],
      { quiet: true },
    );
    const processState = await run(
      [
        "docker",
        "exec",
        CONTAINER,
        "sh",
        "-c",
        "tr '\\0' '\\n' </proc/1/cmdline; tr '\\0' '\\n' </proc/1/environ",
      ],
      { quiet: true },
    );
    if (`${inspection.stdout}${processState.stdout}`.includes(secret)) {
      throw new Error("TURN auth secret leaked into container metadata or process state");
    }
    console.log("TURN container, REST authentication, relay path, and secret isolation verified.");
  } finally {
    await run(["docker", "rm", "-f", CONTAINER], { quiet: true, allowFailure: true });
    await rm(workspace, { recursive: true, force: true });
  }
}

await main();
