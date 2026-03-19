# MyCli

`MyCli` is a personal CLI for managing multiple shell sessions from one command.

## Features

- Open and track multiple named terminal sessions
- Re-attach to running sessions from later CLI invocations
- Persist session metadata under `~/.mycli/sessions.json`
- Generate PowerShell completion

## Commands

```powershell
npm run build
npm link
mycli open work --cwd E:\Project
mycli list
mycli attach work
mycli kill work
mycli completion --shell powershell
```

Detach from an attached session with `Ctrl+P`.

## Notes

- The current MVP keeps sessions alive through the background daemon process.
- If the daemon stops, shell processes stop with it.
- PowerShell completion can be loaded by evaluating the output of `mycli completion --shell powershell`.
- On Windows, `pwsh` is preferred when available, then Windows PowerShell, then `cmd.exe`.
