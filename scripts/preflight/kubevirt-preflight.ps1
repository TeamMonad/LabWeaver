[CmdletBinding()]
param(
    [Parameter()]
    [string]$Namespace = "",

    [Parameter()]
    [string]$EvidenceDirectory = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$script:HasFailure = $false

function Protect-Text {
    param([AllowEmptyString()][string]$Text)

    if ([string]::IsNullOrEmpty($Text)) {
        return ""
    }

    $protected = $Text -replace '(?i)[A-Z]:\\Users\\[^\\\s]+', '<USER_HOME>'
    $protected = $protected -replace '(?i)/home/[^/\s]+', '<USER_HOME>'
    $protected = $protected -replace '(?<![0-9])(?:[0-9]{1,3}\.){3}[0-9]{1,3}(?![0-9])', '<REDACTED_IP>'
    $protected = $protected -replace '(?i)(token|secret|password|client-secret)\s*[:=]\s*\S+', '$1=<REDACTED>'
    return $protected
}

function Write-CheckStatus {
    param(
        [ValidateSet("PASS", "FAIL", "BLOCKED")]
        [string]$Status,
        [string]$Name,
        [string]$Detail = ""
    )

    if ($Status -ne "PASS") {
        $script:HasFailure = $true
    }

    $safeDetail = Protect-Text $Detail
    if ([string]::IsNullOrWhiteSpace($safeDetail)) {
        Write-Host "[$Status] $Name"
    }
    else {
        Write-Host "[$Status] $Name - $safeDetail"
    }
}

function Add-RawLog {
    param(
        [string]$Label,
        [string]$Command,
        [int]$ExitCode,
        [AllowEmptyString()][string]$Output
    )

    $entry = @(
        "",
        "=== $Label ===",
        "Command: $Command",
        "Exit code: $ExitCode",
        "Output:",
        $Output
    ) -join [Environment]::NewLine
    Add-Content -LiteralPath $script:RawLogPath -Value $entry -Encoding UTF8
}

function Invoke-LoggedCommand {
    param(
        [string]$Label,
        [string]$FilePath,
        [string[]]$Arguments,
        [ValidateSet("FAIL", "BLOCKED")]
        [string]$FailureStatus = "FAIL"
    )

    try {
        $lines = @(& $FilePath @Arguments 2>&1 | ForEach-Object { $_.ToString() })
        $exitCode = $LASTEXITCODE
        if ($null -eq $exitCode) {
            $exitCode = 0
        }
        $output = $lines -join [Environment]::NewLine
    }
    catch {
        $exitCode = 1
        $output = $_.Exception.Message
    }

    $renderedCommand = if ($Arguments.Count -gt 0) {
        "$FilePath $($Arguments -join ' ')"
    }
    else {
        $FilePath
    }
    Add-RawLog -Label $Label -Command $renderedCommand -ExitCode $exitCode -Output $output

    if ($exitCode -eq 0) {
        Write-CheckStatus -Status PASS -Name $Label
    }
    else {
        $diagnostic = ($output -split "`r?`n" | Select-Object -Last 3) -join " | "
        Write-CheckStatus -Status $FailureStatus -Name $Label -Detail $diagnostic
    }

    return [pscustomobject]@{
        ExitCode = $exitCode
        Output   = $output
    }
}

function Get-ShortHash {
    param([string]$Value)

    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
        $hash = $sha256.ComputeHash($bytes)
        return ([System.BitConverter]::ToString($hash) -replace '-', '').Substring(0, 8).ToLowerInvariant()
    }
    finally {
        $sha256.Dispose()
    }
}

function Test-OutputMatch {
    param(
        [string]$Name,
        [AllowEmptyString()][string]$Output,
        [string]$Pattern,
        [string]$SuccessDetail,
        [string]$FailureDetail
    )

    if ($Output -match $Pattern) {
        Write-CheckStatus -Status PASS -Name $Name -Detail $SuccessDetail
    }
    else {
        Write-CheckStatus -Status FAIL -Name $Name -Detail $FailureDetail
    }
}

$scriptPath = $MyInvocation.MyCommand.Path
$scriptDirectory = Split-Path -Parent $scriptPath
$repositoryRoot = (Resolve-Path (Join-Path $scriptDirectory "..\..")).Path

if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $repositoryRoot "docs\testing\evidence\local"
}
elseif (-not [System.IO.Path]::IsPathRooted($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $repositoryRoot $EvidenceDirectory
}

New-Item -ItemType Directory -Path $EvidenceDirectory -Force | Out-Null
$startedAt = Get-Date
$timestamp = $startedAt.ToUniversalTime().ToString("yyyyMMddTHHmmssZ")
$script:RawLogPath = Join-Path $EvidenceDirectory "vm-01a-preflight-$timestamp.raw.log"

$gitSha = "<unknown>"
try {
    $gitShaOutput = @(& git -C $repositoryRoot rev-parse HEAD 2>&1)
    if ($LASTEXITCODE -eq 0 -and $gitShaOutput.Count -gt 0) {
        $gitSha = $gitShaOutput[0].ToString().Trim()
    }
}
catch {
    $gitSha = "<unknown>"
    Add-Content -LiteralPath $script:RawLogPath -Value ("Git SHA diagnostic: " + $_.Exception.Message) -Encoding UTF8
}

@(
    "LabWeaver VM-01a KubeVirt and storage preflight",
    "Evidence time (UTC): $($startedAt.ToUniversalTime().ToString('o'))",
    "Git commit: $gitSha",
    "Namespace: $(if ([string]::IsNullOrWhiteSpace($Namespace)) { '<all-namespaces>' } else { $Namespace })"
) | Set-Content -LiteralPath $script:RawLogPath -Encoding UTF8

Write-Host "LabWeaver VM-01a read-only preflight"
Write-Host "Evidence time (UTC): $($startedAt.ToUniversalTime().ToString('o'))"
Write-Host "Git commit: $gitSha"
Write-Host "Namespace: $(if ([string]::IsNullOrWhiteSpace($Namespace)) { '<all-namespaces>' } else { $Namespace })"
Write-Host "Raw log: docs/testing/evidence/local/$([System.IO.Path]::GetFileName($script:RawLogPath))"

$prerequisiteBlocked = $false
$kubectlCommand = Get-Command kubectl -ErrorAction SilentlyContinue
if ($null -eq $kubectlCommand) {
    Write-CheckStatus -Status BLOCKED -Name "kubectl prerequisite" -Detail "kubectl is not available on PATH"
    Add-RawLog -Label "kubectl prerequisite" -Command "Get-Command kubectl" -ExitCode 1 -Output "kubectl is not available on PATH"
    $prerequisiteBlocked = $true
}
else {
    $kubectlVersion = Invoke-LoggedCommand -Label "kubectl version --client" -FilePath "kubectl" -Arguments @("version", "--client") -FailureStatus BLOCKED
    if ($kubectlVersion.ExitCode -ne 0) {
        $prerequisiteBlocked = $true
    }
}

$virtctlCommand = Get-Command virtctl -ErrorAction SilentlyContinue
if ($null -eq $virtctlCommand) {
    Write-CheckStatus -Status BLOCKED -Name "virtctl prerequisite" -Detail "virtctl is not available on PATH"
    Add-RawLog -Label "virtctl prerequisite" -Command "Get-Command virtctl" -ExitCode 1 -Output "virtctl is not available on PATH"
    $prerequisiteBlocked = $true
}
else {
    $virtctlVersion = Invoke-LoggedCommand -Label "virtctl version" -FilePath "virtctl" -Arguments @("version") -FailureStatus BLOCKED
    if ($virtctlVersion.ExitCode -ne 0) {
        $prerequisiteBlocked = $true
    }
}

$helmCommand = Get-Command helm -ErrorAction SilentlyContinue
if ($null -eq $helmCommand) {
    Write-CheckStatus -Status BLOCKED -Name "helm version" -Detail "helm is not available on PATH"
    Add-RawLog -Label "helm version" -Command "Get-Command helm" -ExitCode 1 -Output "helm is not available on PATH"
}
else {
    $null = Invoke-LoggedCommand -Label "helm version" -FilePath "helm" -Arguments @("version") -FailureStatus BLOCKED
}

$context = ""
if ($null -ne $kubectlCommand) {
    $contextResult = Invoke-LoggedCommand -Label "kube context" -FilePath "kubectl" -Arguments @("config", "current-context") -FailureStatus BLOCKED
    $context = $contextResult.Output.Trim()
    if ($contextResult.ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($context)) {
        $prerequisiteBlocked = $true
    }
    else {
        Write-Host "Redacted context: <context:$(Get-ShortHash $context)>"
        $contextsResult = Invoke-LoggedCommand -Label "kubectl config get-contexts" -FilePath "kubectl" -Arguments @("config", "get-contexts") -FailureStatus BLOCKED
        if ($contextsResult.ExitCode -ne 0) {
            $prerequisiteBlocked = $true
        }
    }
}
else {
    Write-CheckStatus -Status BLOCKED -Name "kube context" -Detail "not checked because kubectl is unavailable"
    $prerequisiteBlocked = $true
}

if ($prerequisiteBlocked) {
    Write-CheckStatus -Status BLOCKED -Name "Cluster preflight" -Detail "local tools or kube context are incomplete; no cluster queries were run"
    exit 2
}

$clusterInfo = Invoke-LoggedCommand -Label "Kubernetes API" -FilePath "kubectl" -Arguments @("cluster-info")
$nodesWide = Invoke-LoggedCommand -Label "Nodes" -FilePath "kubectl" -Arguments @("get", "nodes", "-o", "wide")
$nodeReadiness = Invoke-LoggedCommand -Label "Node readiness summary" -FilePath "kubectl" -Arguments @(
    "get",
    "nodes",
    "-o",
    'custom-columns=READY:.status.conditions[?(@.type=="Ready")].status',
    "--no-headers"
)
if ($nodeReadiness.ExitCode -eq 0) {
    $nodeStatuses = @($nodeReadiness.Output -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $readyCount = @($nodeStatuses | Where-Object { $_.Trim() -eq "True" }).Count
    if ($nodeStatuses.Count -gt 0 -and $readyCount -eq $nodeStatuses.Count) {
        Write-CheckStatus -Status PASS -Name "Node Ready state" -Detail "$readyCount/$($nodeStatuses.Count) nodes Ready"
    }
    else {
        Write-CheckStatus -Status FAIL -Name "Node Ready state" -Detail "$readyCount/$($nodeStatuses.Count) nodes Ready"
    }
}
$null = Invoke-LoggedCommand -Label "Node read permission" -FilePath "kubectl" -Arguments @("auth", "can-i", "get", "nodes")
$null = Invoke-LoggedCommand -Label "VM read permission" -FilePath "kubectl" -Arguments @("auth", "can-i", "get", "virtualmachines.kubevirt.io", "--all-namespaces")
$null = Invoke-LoggedCommand -Label "VMI read permission" -FilePath "kubectl" -Arguments @("auth", "can-i", "get", "virtualmachineinstances.kubevirt.io", "--all-namespaces")

$nodesLabels = Invoke-LoggedCommand -Label "Node labels" -FilePath "kubectl" -Arguments @("get", "nodes", "--show-labels")
if ($nodesLabels.ExitCode -eq 0) {
    Test-OutputMatch -Name "kubevirt.io/schedulable" -Output $nodesLabels.Output -Pattern 'kubevirt\.io/schedulable' -SuccessDetail "schedulable label observed" -FailureDetail "schedulable label not observed"
}

$kvmResources = Invoke-LoggedCommand -Label "KVM capacity and allocatable" -FilePath "kubectl" -Arguments @(
    "get",
    "nodes",
    "-o",
    'custom-columns=NAME:.metadata.name,KVM_CAPACITY:.status.capacity.devices\.kubevirt\.io/kvm,KVM_ALLOCATABLE:.status.allocatable.devices\.kubevirt\.io/kvm',
    "--no-headers"
)
if ($kvmResources.ExitCode -eq 0) {
    $kvmCapableNodeFound = $false
    foreach ($line in @($kvmResources.Output -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })) {
        $columns = @($line.Trim() -split '\s+')
        if ($columns.Count -lt 3 -or $columns[1] -eq "<none>" -or $columns[2] -eq "<none>") {
            continue
        }

        $capacity = 0L
        $allocatable = 0L
        $capacityIsValid = [long]::TryParse($columns[1], [ref]$capacity)
        $allocatableIsValid = [long]::TryParse($columns[2], [ref]$allocatable)
        if ($capacityIsValid -and $allocatableIsValid -and $capacity -gt 0 -and $allocatable -gt 0) {
            $kvmCapableNodeFound = $true
            break
        }
    }

    if ($kvmCapableNodeFound) {
        Write-CheckStatus -Status PASS -Name "devices.kubevirt.io/kvm" -Detail "at least one node has non-zero capacity and allocatable"
    }
    else {
        Write-CheckStatus -Status FAIL -Name "devices.kubevirt.io/kvm" -Detail "no node has valid non-zero capacity and allocatable"
    }
}

$crds = Invoke-LoggedCommand -Label "KubeVirt CRDs" -FilePath "kubectl" -Arguments @("get", "crd")
if ($crds.ExitCode -eq 0) {
    Test-OutputMatch -Name "VirtualMachine CRD" -Output $crds.Output -Pattern 'virtualmachines\.kubevirt\.io' -SuccessDetail "CRD observed" -FailureDetail "CRD not observed"
    Test-OutputMatch -Name "VirtualMachineInstance CRD" -Output $crds.Output -Pattern 'virtualmachineinstances\.kubevirt\.io' -SuccessDetail "CRD observed" -FailureDetail "CRD not observed"
}

$kubevirt = Invoke-LoggedCommand -Label "KubeVirt custom resource" -FilePath "kubectl" -Arguments @("get", "kubevirt", "-A")
if ($kubevirt.ExitCode -eq 0) {
    Test-OutputMatch -Name "KubeVirt installation" -Output $kubevirt.Output -Pattern '(?m)^(?!No resources found)(?!NAMESPACE\s+NAME).+\S' -SuccessDetail "KubeVirt resource observed" -FailureDetail "no KubeVirt resource observed"
}

$pods = Invoke-LoggedCommand -Label "Cluster pods" -FilePath "kubectl" -Arguments @("get", "pods", "-A")
if ($pods.ExitCode -eq 0) {
    foreach ($component in @("virt-operator", "virt-controller", "virt-handler", "virt-api")) {
        Test-OutputMatch -Name $component -Output $pods.Output -Pattern ([regex]::Escape($component)) -SuccessDetail "pod observed" -FailureDetail "pod not observed"
    }
}

$scopeArguments = if ([string]::IsNullOrWhiteSpace($Namespace)) { @("-A") } else { @("-n", $Namespace) }
$vmArguments = @("get", "vm,vmi") + $scopeArguments
$null = Invoke-LoggedCommand -Label "VM and VMI state" -FilePath "kubectl" -Arguments $vmArguments

$cdi = Invoke-LoggedCommand -Label "CDI custom resource" -FilePath "kubectl" -Arguments @("get", "cdi", "-A")
if ($cdi.ExitCode -eq 0) {
    Test-OutputMatch -Name "CDI installation" -Output $cdi.Output -Pattern '(?m)^(?!No resources found)(?!NAMESPACE\s+NAME).+\S' -SuccessDetail "CDI resource observed" -FailureDetail "no CDI resource observed"
}
$dataVolumeArguments = @("get", "datavolume") + $scopeArguments
$null = Invoke-LoggedCommand -Label "DataVolume API" -FilePath "kubectl" -Arguments $dataVolumeArguments

$storageClasses = Invoke-LoggedCommand -Label "StorageClass list" -FilePath "kubectl" -Arguments @("get", "storageclass")
$storageYaml = Invoke-LoggedCommand -Label "StorageClass YAML" -FilePath "kubectl" -Arguments @("get", "storageclass", "-o", "yaml")
$storageSummary = Invoke-LoggedCommand -Label "StorageClass summary" -FilePath "kubectl" -Arguments @(
    "get",
    "storageclass",
    "-o",
    'custom-columns=NAME:.metadata.name,PROVISIONER:.provisioner,DEFAULT:.metadata.annotations.storageclass\.kubernetes\.io/is-default-class,VOLUME_BINDING_MODE:.volumeBindingMode,ALLOW_VOLUME_EXPANSION:.allowVolumeExpansion,RECLAIM_POLICY:.reclaimPolicy',
    "--no-headers"
)
if ($storageClasses.ExitCode -eq 0) {
    Test-OutputMatch -Name "StorageClass availability" -Output $storageClasses.Output -Pattern '(?m)^(?!No resources found)(?!NAME\s+PROVISIONER).+\S' -SuccessDetail "StorageClass observed" -FailureDetail "no StorageClass observed"
}
if ($storageSummary.ExitCode -eq 0) {
    foreach ($summaryLine in @($storageSummary.Output -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })) {
        Write-CheckStatus -Status PASS -Name "StorageClass fields" -Detail $summaryLine
    }
}

$ingressClasses = Invoke-LoggedCommand -Label "IngressClass" -FilePath "kubectl" -Arguments @("get", "ingressclass")
$ingressArguments = @("get", "ingress") + $scopeArguments
$ingresses = Invoke-LoggedCommand -Label "Ingress resources" -FilePath "kubectl" -Arguments $ingressArguments
if ($ingressClasses.ExitCode -eq 0 -and $ingresses.ExitCode -eq 0 -and $pods.ExitCode -eq 0) {
    $ingressEvidence = $ingressClasses.Output + [Environment]::NewLine + $ingresses.Output + [Environment]::NewLine + $pods.Output
    Test-OutputMatch -Name "Ingress components" -Output $ingressEvidence -Pattern '(?i)ingress|nginx|traefik' -SuccessDetail "matching resource observed" -FailureDetail "no ingress, nginx, or traefik resource observed"
}

$headscale = Invoke-LoggedCommand -Label "Headscale resources" -FilePath "kubectl" -Arguments @("get", "deploy,statefulset,service,pods", "-A")
if ($headscale.ExitCode -eq 0) {
    Test-OutputMatch -Name "Headscale" -Output $headscale.Output -Pattern '(?i)headscale' -SuccessDetail "matching resource observed" -FailureDetail "no headscale resource observed"
}

if ($script:HasFailure) {
    Write-CheckStatus -Status FAIL -Name "Preflight result" -Detail "one or more checks failed or were blocked"
    exit 1
}

Write-CheckStatus -Status PASS -Name "Preflight result" -Detail "all read-only checks passed"
exit 0
