export interface SessionRecord {
  name: string;
  shell: string;
  cwd: string;
  pid: number;
  createdAt: string;
  updatedAt: string;
  status: "running" | "attached" | "stopped";
}

export interface PersistedState {
  sessions: SessionRecord[];
}

export type ClientMessage =
  | {
      type: "createSession";
      name: string;
      shell?: string;
      cwd?: string;
    }
  | {
      type: "listSessions";
    }
  | {
      type: "killSession";
      name: string;
    }
  | {
      type: "attachSession";
      name: string;
      cols: number;
      rows: number;
    }
  | {
      type: "stdin";
      data: string;
    }
  | {
      type: "resize";
      cols: number;
      rows: number;
    }
  | {
      type: "detach";
    }
  | {
      type: "health";
    };

export type ServerMessage =
  | {
      type: "ready";
      pid: number;
    }
  | {
      type: "success";
      message: string;
      session?: SessionRecord;
      sessions?: SessionRecord[];
    }
  | {
      type: "error";
      message: string;
    }
  | {
      type: "attached";
      session: SessionRecord;
    }
  | {
      type: "output";
      data: string;
    }
  | {
      type: "sessionExit";
      name: string;
      exitCode: number;
    };
