#!/usr/bin/env node
import { Command } from "commander";
import process from "node:process";
import { attachSession, request } from "./client.js";
import { renderPowerShellCompletion } from "./completion.js";
import { resolveDefaultShell } from "./config.js";
import type { ServerMessage } from "./types.js";

const program = new Command();

program
  .name("mycli")
  .description("Personal terminal session manager")
  .version("0.1.0");

program
  .command("open")
  .argument("<name>", "session name")
  .option("--cwd <path>", "working directory", process.cwd())
  .option("--shell <shell>", "shell executable", resolveDefaultShell())
  .action(async (name, options) => {
    const response = await request({
      type: "createSession",
      name,
      cwd: options.cwd,
      shell: options.shell,
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

    for (const session of sessions) {
      process.stdout.write(
        `${session.name}\t${session.status}\t${session.shell}\t${session.cwd}\n`,
      );
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
