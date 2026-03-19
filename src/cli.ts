#!/usr/bin/env node
import { Command } from "commander";
import process from "node:process";
import { attachSession, daemonStatus, ensureDaemon, request, stopDaemon } from "./client.js";
import { renderPowerShellCompletion } from "./completion.js";
import { resolveDefaultShell } from "./config.js";
import { getProfile, loadProjectConfig } from "./project-config.js";
import type { ServerMessage } from "./types.js";

const program = new Command();

program
  .name("mycli")
  .description("Personal terminal session manager")
  .version("0.1.0");

program
  .command("open")
  .argument("<name>", "session name")
  .option("--cwd <path>", "working directory")
  .option("--shell <shell>", "shell executable")
  .option("--profile <name>", "profile from mycli.config.json")
  .option("--env <key=value>", "environment variable override", collectValues, [])
  .action(async (name, options) => {
    const config = loadProjectConfig(process.cwd());
    const profile = getProfile(config, options.profile);

    if (options.profile && !profile) {
      throw new Error(`Profile '${options.profile}' not found in mycli.config.json.`);
    }

    const cwd = options.cwd ?? profile?.cwd ?? process.cwd();
    const shell = options.shell ?? profile?.shell ?? resolveDefaultShell();
    const env = {
      ...(profile?.env ?? {}),
      ...parseEnvEntries(options.env),
    };

    const response = await request({
      type: "createSession",
      name,
      cwd,
      shell,
      profileName: options.profile,
      env: Object.keys(env).length > 0 ? env : undefined,
    });

    assertSuccess(response);
    process.stdout.write(`${response.message}\n`);
  });

program
  .command("list")
  .option("--json", "print JSON output")
  .action(async (options) => {
    const response = await request({
      type: "listSessions",
    });

    assertSuccess(response);

    const sessions = response.sessions ?? [];
    if (options.json) {
      process.stdout.write(`${JSON.stringify(sessions, null, 2)}\n`);
      return;
    }

    if (sessions.length === 0) {
      process.stdout.write("No active sessions.\n");
      return;
    }

    process.stdout.write("NAME\tSTATUS\tPID\tSHELL\tCWD\n");
    for (const session of sessions) {
      process.stdout.write(
        `${session.name}\t${session.status}\t${session.pid}\t${session.shell}\t${session.cwd}\n`,
      );
    }
  });

program
  .command("profiles")
  .option("--json", "print JSON output")
  .action((options) => {
    const config = loadProjectConfig(process.cwd());
    const profileNames = Object.keys(config.profiles ?? {}).sort();

    if (options.json) {
      process.stdout.write(`${JSON.stringify(profileNames, null, 2)}\n`);
      return;
    }

    if (profileNames.length === 0) {
      process.stdout.write("No profiles found in mycli.config.json.\n");
      return;
    }

    for (const name of profileNames) {
      process.stdout.write(`${name}\n`);
    }
  });

program
  .command("attach")
  .argument("<name>", "session name")
  .description("Attach to a session. Press Ctrl+P to detach.")
  .action(async (name) => {
    await attachSession(name);
  });

program
  .command("kill")
  .argument("<name>", "session name")
  .action(async (name) => {
    const response = await request({
      type: "killSession",
      name,
    });

    assertSuccess(response);
    process.stdout.write(`${response.message}\n`);
  });

program
  .command("restore")
  .description("Restore saved sessions into the daemon")
  .action(async () => {
    const response = await request({
      type: "restoreSessions",
    });

    assertSuccess(response);
    process.stdout.write(`${response.message}\n`);
  });

program
  .command("logs")
  .argument("<name>", "session name")
  .option("--lines <number>", "number of lines to print", "50")
  .option("--clean", "strip ANSI escape sequences")
  .action(async (name, options) => {
    const lines = Number.parseInt(options.lines, 10);
    if (!Number.isFinite(lines) || lines <= 0) {
      throw new Error("--lines must be a positive integer.");
    }

    const response = await request({
      type: "readLogs",
      name,
      lines,
      clean: Boolean(options.clean),
    });

    assertSuccess(response);
    process.stdout.write(`${response.log ?? ""}\n`);
  });

const daemon = program.command("daemon").description("Manage the background daemon");

daemon.command("status").action(async () => {
  const response = await daemonStatus();
  const sessionCount = response.sessions?.length ?? 0;
  process.stdout.write(`running\tpid=${response.pid}\tsessions=${sessionCount}\n`);
});

daemon.command("stop").action(async () => {
  const response = await stopDaemon();
  process.stdout.write(`${response.message}\n`);
});

daemon.command("restart").action(async () => {
  try {
    await stopDaemon();
  } catch {
    // The daemon may not be running yet.
  }

  await ensureDaemon();
  const status = await daemonStatus();
  process.stdout.write(`running\tpid=${status.pid}\tsessions=${status.sessions?.length ?? 0}\n`);
});

program
  .command("completion")
  .option("--shell <shell>", "shell type", "powershell")
  .action((options) => {
    if (options.shell !== "powershell") {
      throw new Error("Only PowerShell completion is implemented in this MVP.");
    }

    process.stdout.write(`${renderPowerShellCompletion()}\n`);
  });

program.parseAsync(process.argv).catch((error: Error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});

function assertSuccess(response: ServerMessage): asserts response is Extract<
  ServerMessage,
  { type: "success" }
> {
  if (response.type === "error") {
    throw new Error(response.message);
  }

  if (response.type !== "success") {
    throw new Error("Unexpected daemon response.");
  }
}

function collectValues(value: string, previous: string[]): string[] {
  return [...previous, value];
}

function parseEnvEntries(entries: string[]): Record<string, string> {
  const env: Record<string, string> = {};

  for (const entry of entries) {
    const separatorIndex = entry.indexOf("=");
    if (separatorIndex <= 0) {
      throw new Error(`Invalid env entry '${entry}'. Expected KEY=value.`);
    }

    const key = entry.slice(0, separatorIndex).trim();
    const value = entry.slice(separatorIndex + 1);
    if (!key) {
      throw new Error(`Invalid env entry '${entry}'. Expected KEY=value.`);
    }

    env[key] = value;
  }

  return env;
}
