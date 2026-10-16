# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0

# Run with Windows PowerShell 5.1 (powershell.exe), which supports WinRT projection.
# A muted MediaPlayer still needs a functioning audio endpoint for audio-only files.
$ErrorActionPreference = 'Stop'

foreach ($name in @('AudioEndpointBuilder', 'Audiosrv')) {
    $service = Get-Service -Name $name
    if ($service.StartType -eq 'Disabled') {
        Set-Service -Name $name -StartupType Manual
    }
    Start-Service -Name $name
    (Get-Service -Name $name) | Format-Table Name, Status, StartType
}

try {
    $null = [Windows.Media.Devices.MediaDevice, Windows.Media.Devices, ContentType = WindowsRuntime]
    $null = [Windows.Media.Devices.AudioDeviceRole, Windows.Media.Devices, ContentType = WindowsRuntime]
    $endpoint = [Windows.Media.Devices.MediaDevice]::GetDefaultAudioRenderId(
        [Windows.Media.Devices.AudioDeviceRole]::Default)
    if ([string]::IsNullOrEmpty($endpoint)) {
        Write-Warning 'No default audio render endpoint. Audio-only playback may fail even when video plays. See docs/validation.md.'
    } else {
        Write-Host "Default audio render endpoint: $endpoint"
    }
} catch {
    # Keep the walkthrough running so its native MediaFailed HRESULT is captured.
    Write-Warning "Cannot query the default audio render endpoint: $_"
}
# Record hardware as well as the default endpoint: a sound-device entry alone does
# not prove that MediaPlayer can open an active render endpoint.
Get-CimInstance Win32_SoundDevice | Format-Table Name, Status, ConfigManagerErrorCode
