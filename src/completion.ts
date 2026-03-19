export function renderPowerShellCompletion(): string {
  return `
Register-ArgumentCompleter -Native -CommandName mycli -ScriptBlock {
  param($wordToComplete, $commandAst, $cursorPosition)

  $commands = @('init', 'open', 'list', 'attach', 'kill', 'restore', 'logs', 'daemon', 'profiles', 'completion')
  $commandElements = $commandAst.CommandElements | ForEach-Object { $_.Extent.Text }

  if ($commandElements.Count -le 2) {
    $commands |
      Where-Object { $_ -like "$wordToComplete*" } |
      ForEach-Object {
        [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
      }
    return
  }

  $subcommand = $commandElements[1]
  if ($subcommand -in @('attach', 'kill', 'logs')) {
    try {
      $sessions = mycli list --json | ConvertFrom-Json
      $sessions |
        ForEach-Object { $_.name } |
        Where-Object { $_ -like "$wordToComplete*" } |
        ForEach-Object {
          [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
        }
    } catch {
    }
  }

  if ($subcommand -eq 'open') {
    try {
      $profiles = mycli profiles --json | ConvertFrom-Json
      if ($commandElements -contains '--profile') {
        $profiles |
          Where-Object { $_ -like "$wordToComplete*" } |
          ForEach-Object {
            [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_)
          }
      }
    } catch {
    }
  }
}
`.trim();
}
