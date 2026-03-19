import net from "node:net";

export function sendJson(socket: net.Socket, payload: unknown): void {
  socket.write(`${JSON.stringify(payload)}\n`);
}

export function createJsonLineReader(
  onMessage: (message: unknown) => void,
): (chunk: Buffer | string) => void {
  let buffer = "";

  return (chunk: Buffer | string) => {
    buffer += chunk.toString();

    while (true) {
      const newlineIndex = buffer.indexOf("\n");
      if (newlineIndex === -1) {
        break;
      }

      const line = buffer.slice(0, newlineIndex).trim();
      buffer = buffer.slice(newlineIndex + 1);

      if (!line) {
        continue;
      }

      onMessage(JSON.parse(line));
    }
  };
}
