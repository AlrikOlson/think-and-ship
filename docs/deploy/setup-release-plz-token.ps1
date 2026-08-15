<#
.SYNOPSIS
Idempotently configure the RELEASE_PLZ_TOKEN repo secret on Windows.

.DESCRIPTION
The PowerShell twin of setup-release-plz-token.sh, for Windows users who do
not want a bash in the loop. Running the .sh from PowerShell works, but `bash`
may resolve to WSL's bash depending on PATH, which drags a whole Linux
environment into a two-command task. This script uses only gh and PowerShell.

Why the token is needed: release-plz pushes the `v<version>` tag, and GitHub
does NOT fire downstream workflows (release.yml: binaries + npm) for a tag
created with the default GITHUB_TOKEN. So release-plz must authenticate with a
PAT. Without it, release-plz.yml now stops rather than publishing a half
release — see docs/RELEASING.md.

GitHub has no API to create a PAT, so the value comes from you (a 30-second
web step) or from -Token; everything else is automated.

.PARAMETER Token
The PAT value. Omit it to be prompted with hidden input.

.PARAMETER Force
Replace the secret if it already exists. Without this, an existing secret is
left alone and the script exits successfully.

.EXAMPLE
.\docs\deploy\setup-release-plz-token.ps1
Opens the pre-filled PAT page, then prompts for the value.

.EXAMPLE
.\docs\deploy\setup-release-plz-token.ps1 -Token ghp_xxx -Force
Non-interactive replace.
#>
[CmdletBinding()]
param(
    [string]$Token,
    [switch]$Force
)

# Deliberately NOT 'Stop'. Under PowerShell 5.1 a native command that writes
# anything to stderr raises NativeCommandError when ErrorActionPreference is
# Stop, and `gh api` writes "Not Found (HTTP 404)" there on the entirely
# expected does-this-secret-exist probe. Exit codes are checked explicitly
# instead, which is what actually indicates failure here.
$ErrorActionPreference = 'Continue'
$Secret = 'RELEASE_PLZ_TOKEN'

function Fail($message) {
    Write-Host $message -ForegroundColor Red
    exit 1
}

# Run gh with every stream captured, so an expected non-zero (a 404 probe)
# stays data rather than becoming console noise the user reads as a crash.
function Invoke-Gh {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$GhArgs)
    $output = & gh @GhArgs 2>&1
    return [pscustomobject]@{ ExitCode = $LASTEXITCODE; Output = $output }
}

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    Fail 'gh CLI not found - install from https://cli.github.com'
}

if ((Invoke-Gh auth status).ExitCode -ne 0) {
    Fail 'gh is not authenticated - run: gh auth login'
}

# Resolve the repo from the current checkout, falling back to the canonical one.
$Repo = $env:THINK_AND_SHIP_REPO
if (-not $Repo) {
    $view = Invoke-Gh repo view --json nameWithOwner -q .nameWithOwner
    if ($view.ExitCode -eq 0 -and $view.Output) { $Repo = ($view.Output | Select-Object -First 1) }
    if (-not $Repo) { $Repo = 'AlrikOlson/think-and-ship' }
}
$Repo = "$Repo".Trim()

# Ask the API rather than parsing `gh secret list`: that is a human table whose
# columns vary by gh version. 200 = exists, 404 = does not.
function Test-SecretExists {
    return ((Invoke-Gh api "repos/$Repo/actions/secrets/$Secret").ExitCode -eq 0)
}

if (-not $Force -and (Test-SecretExists)) {
    Write-Host "OK  $Secret already set on $Repo - nothing to do (use -Force to replace)."
    exit 0
}

if (-not $Token) {
    $url = "https://github.com/settings/tokens/new?scopes=repo,workflow&description=release-plz%20($($Repo.Split('/')[-1]))"
    Write-Host "Create a classic PAT with the 'repo' + 'workflow' scopes (pre-filled):"
    Write-Host "  $url"
    try { Start-Process $url } catch { Write-Host '  (could not open a browser - open the URL above yourself)' }

    $secure = Read-Host -Prompt 'Paste the token (input hidden), then Enter' -AsSecureString
    $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try {
        $Token = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr)
    } finally {
        [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr)
    }
}

# A pasted token can arrive with stray whitespace or a carriage return. Stored
# that way it fails authentication with no useful error, which is the worst
# way for this to go wrong.
if ($Token) { $Token = $Token.Trim() }
if (-not $Token) { Fail 'no token provided - aborting.' }

# Value over stdin, never argv, so it stays out of the process list and history.
$Token | & gh secret set $Secret --repo $Repo
if ($LASTEXITCODE -ne 0) { Fail "gh secret set failed (exit $LASTEXITCODE)." }

if (Test-SecretExists) {
    Write-Host "OK  $Secret configured on $Repo." -ForegroundColor Green
    Write-Host '    release-plz will now push tags that trigger release.yml (binaries + npm).'
} else {
    Fail "gh reported success but $Secret does not exist on $Repo - check: gh api repos/$Repo/actions/secrets/$Secret"
}
