#!/usr/bin/env node
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import pty from "node-pty";
import { APP_DIR, SOCKET_PATH, resolveDefaultShell } from "./config.js";
import { createJsonLineReader, sendJson } from "./protocol.js";
import { loadState, saveState } from "./store.js";
import type { ClientMessage, ServerMessage, SessionRecord } from "./types.js";

interface SessionRuntime {
  record: SessionRecord;
  ptyProcess: pty.IPty;
  attachedClient?: net.Socket;
}

const sessions = new Map<string, SessionRuntime>();

bootstrap();

function bootstrap(): void {
  fs.mkdirSync(APP_DIR, { recursive: true });

  if (process.platform !== "win32" && fs.existsSync(SOCKET_PATH)) {
    fs.rmSync(SOCKET_PATH, { force: true });
  }

  restoreKnownSessions();

  const server = net.createServer((socket) => {
    const readJson = createJsonLineReader((message) => {
      handleMessage(socket, message as ClientMessage);
    });

    socket.on("data", readJson);
    socket.on("error", () => {
      detachSocket(socket);
    });
    socket.on("close", () => {
      detachSocket(socket);
    });
  });

  server.listen(SOCKET_PATH, () => {
    const payload: ServerMessage = {
      type: "ready",
      pid: process.pid,
    };
    process.stdout.write(`${JSON.stringify(payload)}\n`);
  });
}

function restoreKnownSessions(): void {
  const state = loadState();

  for (const session of state.sessions) {
    if (session.status === "stopped") {
      continue;
    }

    const runtime = spawnSession(session.name, session.cwd, session.shell);
    runtime.record.createdAt = session.createdAt;
    runtime.record.updatedAt = new Date().toISOString();
    persist();
  }
}

function handleMessage(socket: net.Socket, message: ClientMessage): void {
  switch (message.type) {
    case "health":
      sendJson(socket, {
        type: "success",
        message: "daemon is running",
      } satisfies ServerMessage);
      return;
    case "createSession": {
      if (sessions.has(message.name)) {
        sendJson(socket, {
          type: "error",
          message: `Session '${message.name}' already exists.`,
        } satisfies ServerMessage);
        return;
      }

      const runtime = spawnSession(
        message.name,
        message.cwd ?? process.cwd(),
        resolveDefaultShell(message.shell),
      );

      sendJson(socket, {
        type: "success",
        message: `Session '${message.name}' created.`,
        session: runtime.record,
      } satisfies ServerMessage);
      return;
    }
    case "listSessions":
      sendJson(socket, {
        type: "success",
        message: `${sessions.size} sessions`,
        sessions: [...sessions.values()].map((entry) => entry.record),
      } satisfies ServerMessage);
      return;
    case "killSession": {
      const runtime = sessions.get(message.name);

      if (!runtime) {
        sendJson(socket, {
          type: "error",
          message: `Session '${message.name}' not found.`,
        } satisfies ServerMessage);
        return;
      }

      runtime.record.status = "stopped";
      runtime.ptyProcess.kill();
      sessions.delete(message.name);
      persist();

      sendJson(socket, {
        type: "success",
        message: `Session '${message.name}' killed.`,
      } satisfies ServerMessage);
      return;
    }
    case "attachSession": {
      const runtime = sessions.get(message.name);

      if (!runtime) {
        sendJson(socket, {
          type: "error",
          message: `Session '${message.name}' not found.`,
        } satisfies ServerMessage);
        return;
      }

      if (runtime.attachedClient && !runtime.attachedClient.destroyed) {
        sendJson(socket, {
          type: "error",
          message: `Session '${message.name}' is already attached.`,
        } satisfies ServerMessage);
        return;
      }

      runtime.attachedClient = socket;
      runtime.record.status = "attached";
      runtime.record.updatedAt = new Date().toISOString();
      runtime.ptyProcess.resize(message.cols, message.rows);
      persist();

      sendJson(socket, {
        type: "attached",
        session: runtime.record,
      } satisfies ServerMessage);
      return;
    }
    case "stdin": {
      const runtime = findRuntimeBySocket(socket);
      if (runtime) {
        runtime.ptyProcess.write(Buffer.from(message.data, "base64").toString("utf8"));
      }
      return;
    }
    case "resize": {
      const runtime = findRuntimeBySocket(socket);
      if (runtime) {
        runtime.ptyProcess.resize(message.cols, message.rows);
      }
      return;
    }
    case "detach": {
      detachSocket(socket);
      return;
    }
  }
}

function spawnSession(name: string, cwd: string, shell: string): SessionRuntime {
  const ptyProcess = pty.spawn(shell, [], {
    name: "xterm-color",
    cols: 120,
    rows: 30,
    cwd,
    env: {
      ...process.env,
      TERM: "xterm-256color",
    },
  });

  const now = new Date().toISOString();
  const runtime: SessionRuntime = {
    record: {
      name,
      shell,
      cwd,
      pid: ptyProcess.pid,
      createdAt: now,
      updatedAt: now,
      status: "running",
    },
    ptyProcess,
  };

  ptyProcess.onData((data) => {
    if (runtime.attachedClient && !runtime.attachedClient.destroyed) {
      sendJson(runtime.attachedClient, {
        type: "output",
        data: Buffer.from(data, "utf8").toString("base64"),
      } satisfies ServerMessage);
    }
  });

  ptyProcess.onExit(({ exitCode }) => {
    if (runtime.attachedClient && !runtime.attachedClient.destroyed) {
      sendJson(runtime.attachedClient, {
        type: "sessionExit",
        name,
        exitCode,
      } satisfies ServerMessage);
    }

    sessions.delete(name);
    persist();
  });

  sessions.set(name, runtime);
  persist();
  return runtime;
}

function findRuntimeBySocket(socket: net.Socket): SessionRuntime | undefined {
  for (const runtime of sessions.values()) {
    if (runtime.attachedClient === socket) {
      return runtime;
    }
  }

  return undefined;
}

function detachSocket(socket: net.Socket): void {
  const runtime = findRuntimeBySocket(socket);

  if (!runtime) {
    return;
  }

  runtime.attachedClient = undefined;
  runtime.record.status = "running";
  runtime.record.updatedAt = new Date().toISOString();
  persist();
}

function persist(): void {
  saveState([...sessions.values()].map((entry) => entry.record));
}
