$scriptPath = Join-Path $PSScriptRoot "kubevirt-preflight.ps1"
$scriptContent = Get-Content -LiteralPath $scriptPath -Raw

Describe "KubeVirt preflight safety contract" {
    It "parses as valid PowerShell" {
        $tokens = $null
        $errors = $null
        [System.Management.Automation.Language.Parser]::ParseFile(
            $scriptPath,
            [ref]$tokens,
            [ref]$errors
        ) | Out-Null

        $errors.Count | Should Be 0
    }

    It "uses terminating PowerShell errors" {
        $scriptContent | Should Match '\$ErrorActionPreference\s*=\s*"Stop"'
    }

    It "supports an optional namespace" {
        $scriptContent | Should Match '\[string\]\$Namespace\s*=\s*""'
    }

    It "fails fast when local prerequisites are blocked" {
        $scriptContent | Should Match 'if \(\$prerequisiteBlocked\)'
        $scriptContent | Should Match 'exit 2'
    }

    It "writes raw logs only to the local evidence directory by default" {
        $scriptContent | Should Match 'docs\\testing\\evidence\\local'
        $scriptContent | Should Match '\.raw\.log'
    }

    It "renders logged commands without executing a joined command string" {
        $scriptContent | Should Match '\$renderedCommand\s*=\s*if \(\$Arguments\.Count -gt 0\)'
        $scriptContent | Should Match '\$Arguments -join '' '''
        $scriptContent | Should Match '& \$FilePath @Arguments'
        $scriptContent | Should Match 'Add-RawLog -Label \$Label -Command \$renderedCommand'
        $scriptContent | Should Not Match '\(\(\$FilePath, \$Arguments\) -join'
    }

    It "checks KVM capacity and allocatable instead of treating KVM as a label" {
        $scriptContent | Should Match '\.status\.capacity\.devices\\\.kubevirt\\\.io/kvm'
        $scriptContent | Should Match '\.status\.allocatable\.devices\\\.kubevirt\\\.io/kvm'
        $scriptContent | Should Match '\$capacity -gt 0 -and \$allocatable -gt 0'
        $scriptContent | Should Not Match 'Test-OutputMatch -Name "devices\.kubevirt\.io/kvm" -Output \$nodesLabels\.Output'
    }

    It "keeps the schedulable node label check" {
        $scriptContent | Should Match 'Test-OutputMatch -Name "kubevirt\.io/schedulable" -Output \$nodesLabels\.Output'
    }

    It "contains no Kubernetes or VM lifecycle write operation" {
        $scriptContent | Should Not Match '(?i)-Arguments\s+@\("(apply|create|delete|patch|edit|start|stop)"'
        $scriptContent | Should Not Match '(?im)^\s*(kubectl|virtctl)\s+(apply|create|delete|patch|edit|start|stop)\b'
    }
}
