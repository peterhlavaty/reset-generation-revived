# Reset Generation Revived: Windows Firewall setup for LAN games (run LAN-Setup.cmd).
#
# The PC that hosts a game (F5) runs the relay on port 8787: TCP for the game, UDP for the other
# players' search. This lets both in from the local network only (LocalSubnet), in every network
# type, so it does not matter whether Windows calls the network public or private. It also removes
# the inbound Block rules Windows creates for the game when its firewall prompt is cancelled (a
# Block rule wins over any Allow rule). Joining players need nothing, but running it on every PC
# is harmless, and so is running it again. -Remove deletes the rules.
param([switch]$Remove)

$group = 'Reset Generation Revived'
$port = 8787

function Wait-Close {
    Write-Host ''
    Read-Host 'Press Enter to close' | Out-Null
}

$me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $argList = '-NoProfile -ExecutionPolicy Bypass -File "' + $PSCommandPath + '"'
    if ($Remove) { $argList += ' -Remove' }
    try {
        Start-Process -FilePath powershell.exe -Verb RunAs -ArgumentList $argList -ErrorAction Stop
    } catch {
        Write-Host 'Administrator rights are needed to change the Windows Firewall settings.'
        Wait-Close
    }
    exit
}

try {
    $ErrorActionPreference = 'Stop'
    Get-NetFirewallRule -Group $group -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    if ($Remove) {
        Write-Host 'Removed the Reset Generation Revived LAN rules.'
    } else {
        $blocked = Get-NetFirewallRule -Direction Inbound -Action Block -ErrorAction SilentlyContinue |
            Where-Object { ($_ | Get-NetFirewallApplicationFilter).Program -like '*\ResetGeneration-Revived.exe' }
        foreach ($r in $blocked) {
            Remove-NetFirewallRule -Name $r.Name
            Write-Host "Removed a rule that blocked the game: $($r.DisplayName)"
        }
        foreach ($proto in 'TCP', 'UDP') {
            New-NetFirewallRule -Group $group -DisplayName "Reset Generation Revived LAN ($proto $port)" `
                -Direction Inbound -Protocol $proto -LocalPort $port -RemoteAddress LocalSubnet `
                -Profile Any -Action Allow | Out-Null
        }
        Write-Host "Done: this PC can host LAN games (TCP and UDP $port open to the local network)."
        foreach ($p in Get-NetConnectionProfile) {
            Write-Host "  network: $($p.Name) ($($p.InterfaceAlias), $($p.NetworkCategory))"
        }
        if (Get-NetTCPConnection -LocalPort $port -State Listen -ErrorAction SilentlyContinue) {
            Write-Host '  The game is hosting right now: the others can choose Quick Match.'
        } else {
            Write-Host '  To host: start the game and press F5 in the main menu.'
        }
        # Antivirus suites with their own firewall (ESET, Norton, McAfee...) take over from Windows
        # Firewall, so the rules above do not apply there; that needs a rule in the suite itself.
        $others = Get-CimInstance -Namespace root/SecurityCenter2 -ClassName FirewallProduct -ErrorAction SilentlyContinue
        foreach ($name in @($others | ForEach-Object { $_.displayName })) {
            Write-Host ''
            Write-Host "WARNING: $name filters this PC's network instead of Windows Firewall."
            Write-Host "  Allow incoming connections there too: for ResetGeneration-Revived.exe, or TCP and"
            Write-Host "  UDP local port $port from the local network."
            if ($name -like '*ESET*') {
                Write-Host '  ESET: after a player failed to connect, open ESET > Setup > Network protection >'
                Write-Host '  Troubleshooting wizard and unblock ResetGeneration-Revived.exe, or add a rule in'
                Write-Host "  Advanced setup > Firewall > Rules (direction In, Allow, TCP & UDP, local port $port)."
            }
        }
    }
} catch {
    Write-Host "Failed: $_"
}
Wait-Close
