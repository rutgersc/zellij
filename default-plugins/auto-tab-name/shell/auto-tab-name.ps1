# Shell-side contract for the zellij `auto-tab-name` plugin.
#
# The plugin reads PaneInfo.title (set by OSC 0/2 escape sequences) and uses
# it to derive a tab name. Apps like nvim/claude/lazygit emit OSC 0 themselves;
# a plain PowerShell prompt does not. Without this snippet, shell-only panes
# stay nameless and the plugin can't fill the tab name for them.
#
# Dot-source this file from your $PROFILE. The plugin source lives at
# F:/GitRepos/zellij/default-plugins/auto-tab-name/ — keep them moving together.

function global:Set-ZellijTabTitle {
    [CmdletBinding()]
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Title)
    if (-not $env:ZELLIJ) { return }
    if ([string]::IsNullOrWhiteSpace($Title)) { return }
    # OSC 0 ; <text> BEL — sets icon name AND window title.
    [Console]::Write("$([char]27)]0;$Title$([char]7)")
}

# Wrap the existing prompt so the title reflects the cwd whenever we're idle
# at the prompt. Running apps (nvim, claude, etc.) overwrite this with their
# own OSC 0 while they're foregrounded — last writer wins.
$script:__ZellijOriginalPrompt = $function:prompt

function global:prompt {
    Set-ZellijTabTitle -Title "pwsh $(Split-Path -Leaf $PWD)"
    if ($script:__ZellijOriginalPrompt) {
        & $script:__ZellijOriginalPrompt
    } else {
        "PS $($executionContext.SessionState.Path.CurrentLocation)> "
    }
}
