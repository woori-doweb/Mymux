import fs from "node:fs";
import path from "node:path";
import { PROJECT_CONFIG_FILE } from "./config.js";
import type { MyCliConfig, SessionProfile } from "./types.js";

export function loadProjectConfig(cwd: string): MyCliConfig {
  const configPath = path.join(cwd, PROJECT_CONFIG_FILE);
  if (!fs.existsSync(configPath)) {
    return {};
  }

  return JSON.parse(fs.readFileSync(configPath, "utf8")) as MyCliConfig;
}

export function getProfile(config: MyCliConfig, name?: string): SessionProfile | undefined {
  if (!name) {
    return undefined;
  }

  return config.profiles?.[name];
}
