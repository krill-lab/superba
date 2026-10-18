# Superba Windows e2e desktop agent: compiles WtAgent.cs and serves the host's
# requests on 127.0.0.1:<Port> (reached through an SSH local forward). Started by
# start-agent.ps1 as a scheduled task in the interactive desktop session.
param(
    [Parameter(Mandatory = $true)][int]$Port,
    [Parameter(Mandatory = $true)][string]$TokenFile,
    [Parameter(Mandatory = $true)][string]$Root
)
$ErrorActionPreference = 'Stop'
$log = Join-Path $Root 'agent.log'
try {
    $token = (Get-Content -Raw -LiteralPath $TokenFile).Trim()
    Remove-Item -Force -LiteralPath $TokenFile
    $src = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'WtAgent.cs')
    Add-Type -TypeDefinition $src -Language CSharp -ReferencedAssemblies @(
        'UIAutomationClient', 'UIAutomationTypes', 'WindowsBase', 'System.Drawing',
        'System.Management', 'System.Web.Extensions', 'System.Windows.Forms')
    [WtAgent]::Serve($Port, $token, $log)
} catch {
    Add-Content -LiteralPath $log -Value ("agent failed: " + ($_ | Out-String))
    exit 1
}
