# bk shell integration for PowerShell. Add this line to your profile (notepad $PROFILE):
#
#     bk init powershell | Out-String | Invoke-Expression
#
# - remembers the last command you ran (with its folder and active env) for `bk save`
# - `bk go NAME` enters a bookmark's folder + env in the current session
# - tab completion for bookmark names
#
# `bk` stays the real program (a wrapper function would lose a bare `--`, which PowerShell
# swallows when calling functions). Instead, the prompt hook keeps a small per-session file:
# the last command for `bk save`, and a script `bk go` leaves for the prompt to run. Your
# existing prompt (starship, oh-my-posh, ...) keeps working on top.
if (-not (Get-Variable -Name __BkLoaded -Scope Global -ErrorAction SilentlyContinue)) {
  $global:__BkLoaded = $true
  $global:__BkRecord = __BK_RECORD_VALUE__
  $global:__BkRecentFile = __BK_RECENT_FILE__
  $global:__BkSessionDir = __BK_SESSION_DIR__
  $global:__BkHistId = 0
  $global:__BkSnap = @{ Pwd = $PWD.ProviderPath; Conda = "$env:CONDA_DEFAULT_ENV"; Venv = "$env:VIRTUAL_ENV" }
  $global:__BkPrompt = $function:prompt
  $global:__BkUtf8 = [Text.UTF8Encoding]::new($false)

  New-Item -ItemType Directory -Force -Path $global:__BkSessionDir | Out-Null
  if ($IsLinux -or $IsMacOS) { chmod 700 $global:__BkSessionDir }
  $env:BK_PS_SESSION = Join-Path $global:__BkSessionDir "ps-$PID.json"
  $null = Register-EngineEvent -SourceIdentifier PowerShell.Exiting -SupportEvent -Action {
    Remove-Item -LiteralPath $env:BK_PS_SESSION, "$env:BK_PS_SESSION.go" -ErrorAction SilentlyContinue
  }

  if ($global:__BkRecord -and -not (Test-Path -LiteralPath $global:__BkRecentFile)) {
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $global:__BkRecentFile) | Out-Null
    New-Item -ItemType File -Path $global:__BkRecentFile | Out-Null
    if ($IsLinux -or $IsMacOS) { chmod 600 $global:__BkRecentFile }
  }

  function global:__BkRecordLine([string]$Status, [string]$Cmd) {
    if ($Cmd.StartsWith(' ')) { return }
    if ($Cmd -match '(password|passwd|secret|token|api[_-]*key|bearer|authorization)') { return }
    $us = [char]0x1f
    $line = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds().ToString() + $us + $Status + $us + $global:__BkSnap.Pwd +
      $us + $global:__BkSnap.Conda + $us + $global:__BkSnap.Venv + $us + $Cmd + [char]0x1e + "`n"
    [IO.File]::AppendAllText($global:__BkRecentFile, $line, $global:__BkUtf8)
  }

  function global:prompt {
    $ok = $?
    $code = $global:LASTEXITCODE
    # `bk go` left a script for this session: enter the bookmark's folder and env.
    $go = "$env:BK_PS_SESSION.go"
    if (Test-Path -LiteralPath $go) {
      $script = Get-Content -Raw -LiteralPath $go
      Remove-Item -LiteralPath $go -ErrorAction SilentlyContinue
      try { Invoke-Expression $script } catch { Write-Warning "bk go: $_" }
    }
    $h = Get-History -Count 1
    if ($h -and $h.Id -ne $global:__BkHistId) {
      $global:__BkHistId = $h.Id
      $cmd = $h.CommandLine
      $parts = $cmd.Trim() -split '\s+', 2
      if ($parts[0] -notmatch '(^|[\\/])bk(\.exe)?$') {
        $status = if ($ok) { 0 } elseif ($code) { $code } else { 1 }
        $kind = ''
        $expanded = ''
        $c = Get-Command $parts[0] -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($c) {
          $kind = switch ($c.CommandType) {
            'Alias' { 'alias' }
            'Function' { 'function' }
            'Filter' { 'function' }
            'Cmdlet' { 'builtin' }
            default { 'command' }
          }
          if ($kind -eq 'alias') { $expanded = (@($c.Definition) + @($parts | Select-Object -Skip 1)) -join ' ' }
        }
        $last = [ordered]@{
          cmd = $cmd; expanded = $expanded; pwd = $global:__BkSnap.Pwd; status = "$status"
          conda = $global:__BkSnap.Conda; venv = $global:__BkSnap.Venv; kind = $kind
        }
        [IO.File]::WriteAllText($env:BK_PS_SESSION, ($last | ConvertTo-Json -Compress), $global:__BkUtf8)
        if ($global:__BkRecord) { __BkRecordLine "$status" $cmd }
      }
    }
    $global:__BkSnap = @{ Pwd = $PWD.ProviderPath; Conda = "$env:CONDA_DEFAULT_ENV"; Venv = "$env:VIRTUAL_ENV" }
    # Hand the wrapped prompt the command's status, not ours.
    $global:LASTEXITCODE = $code
    if (-not $ok) { Write-Error -Message '' -ErrorAction Ignore }
    if ($global:__BkPrompt) { & $global:__BkPrompt } else { "PS $($PWD.ProviderPath)> " }
  }

  Register-ArgumentCompleter -Native -CommandName bk, bk.exe -ScriptBlock {
    param($wordToComplete, $commandAst, $cursorPosition)
    $words = @($commandAst.CommandElements | ForEach-Object { $_.ToString() })
    $index = if ($wordToComplete) { $words.Count - 1 } else { $words.Count }
    $subs = 'add', 'save', 'run', 'ls', 'show', 'edit', 'rm', 'mv', 'go', 'history', 'doctor', 'trust',
      'export', 'import', 'init', 'app', 'scan', 'server', 'servers', 'ssh'
    $candidates = @()
    if ($index -eq 1) {
      $candidates = @(bk ls --names 2>$null) + $subs
    } elseif ($index -eq 2 -and $words[1] -match '^(run|show|rm|remove|mv|rename|go|edit|history|export)$') {
      $candidates = @(bk ls --names 2>$null)
    }
    $candidates | Where-Object { $_ -like "$wordToComplete*" } |
      ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }
  }
}
