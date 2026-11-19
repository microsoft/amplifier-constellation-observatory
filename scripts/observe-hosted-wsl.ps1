param([Parameter(Mandatory)][string]$Directory)

# This transient read-only subscription records potential WSL ancestors even
# if they exit before the existing window observer can query their image.
$ErrorActionPreference = 'Stop'
$filter = "ProcessName='wsl.exe' OR ProcessName LIKE 'provjobd%' OR ProcessName LIKE 'hosted-compute%' OR ProcessName LIKE 'HostedComputeAgent%'"
$processFilter = $filter.Replace('ProcessName', 'Name')
$identifier = 'ObservatoryHostedProcessStarts'
$path = Join-Path $Directory 'process-starts.jsonl'
$stream = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::Read)
$writer = [IO.StreamWriter]::new($stream, [Text.UTF8Encoding]::new($false))
$writer.AutoFlush = $true
$subscribed = $false
function Write-Observation($Value) {
    $writer.WriteLine(($Value | ConvertTo-Json -Compress -Depth 3))
}

try {
    Register-WmiEvent -Namespace 'root\cimv2' -SourceIdentifier $identifier -Query (
        "SELECT * FROM Win32_ProcessStartTrace WHERE $filter"
    )
    $subscribed = $true
    Write-Observation @{kind='subscribed'; utc=[DateTime]::UtcNow.ToString('o')}
    # Record already-running candidate hosts using only identity fields. Mark
    # their timestamps separately from the WMI process-start event timestamp.
    $existing = @(Get-WmiObject -Namespace 'root\cimv2' -Query (
        "SELECT Name,ProcessId,ParentProcessId,SessionId,CreationDate,ExecutablePath FROM Win32_Process WHERE $processFilter"
    ) | Select-Object -First 64)
    foreach ($item in $existing) {
        $created = [Management.ManagementDateTimeConverter]::ToDateTime($item.CreationDate).ToFileTimeUtc()
        Write-Observation @{kind='initial'; image=$item.Name; image_path=$item.ExecutablePath
            pid=[uint32]$item.ProcessId; parent_pid=[uint32]$item.ParentProcessId
            session=[uint32]$item.SessionId; creation_filetime=[string]$created}
    }
    [IO.File]::WriteAllText((Join-Path $Directory 'ready'), '')
    $until = [DateTime]::UtcNow.AddMinutes(20)
    $count = 0
    while ($count -lt 512 -and [DateTime]::UtcNow -lt $until) {
        $pendingEvent = Wait-Event -SourceIdentifier $identifier -Timeout 1
        if ($null -ne $pendingEvent) {
            $start = $pendingEvent.SourceEventArgs.NewEvent
            Write-Observation @{kind='start'; image=[string]$start.ProcessName
                pid=[uint32]$start.ProcessID; parent_pid=[uint32]$start.ParentProcessID
                session=[uint32]$start.SessionID; event_filetime=[string]$start.TIME_CREATED}
            $count += 1
            Remove-Event -EventIdentifier $pendingEvent.EventIdentifier
        }
        if ([IO.File]::Exists((Join-Path $Directory 'stop')) -and
            $null -eq (Get-Event -SourceIdentifier $identifier -ErrorAction SilentlyContinue | Select-Object -First 1)) {
            break
        }
    }
    Write-Observation @{kind='finished'; count=$count; cap_reached=($count -eq 512)
        owner_finished=[IO.File]::Exists((Join-Path $Directory 'stop'))}
} catch {
    Write-Observation @{kind='unavailable'; error_type=$_.Exception.GetType().Name; hresult=$_.Exception.HResult}
} finally {
    if ($subscribed) { Unregister-Event -SourceIdentifier $identifier -ErrorAction SilentlyContinue }
    $writer.Dispose()
}
