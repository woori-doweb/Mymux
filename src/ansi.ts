const ANSI_PATTERN =
  /\u001B(?:\][^\u0007]*(?:\u0007|\u001B\\)|\[[0-?]*[ -/]*[@-~]|[@-_])/g;

export function stripAnsi(value: string): string {
  return value.replace(ANSI_PATTERN, "");
}
