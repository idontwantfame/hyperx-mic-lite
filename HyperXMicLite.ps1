param(
    [ValidateSet("tray", "list", "status", "mute", "unmute", "toggle", "volume", "default")]
    [string]$Command = "tray",

    [Parameter(Position = 1)]
    [string]$Value
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

namespace HyperXMicLite {
    public enum EDataFlow { eRender = 0, eCapture = 1, eAll = 2 }
    public enum ERole { eConsole = 0, eMultimedia = 1, eCommunications = 2 }
    [Flags] public enum DeviceState { Active = 1, Disabled = 2, NotPresent = 4, Unplugged = 8, All = 15 }

    [ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")]
    public class MMDeviceEnumeratorComObject { }

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IMMDeviceEnumerator {
        int EnumAudioEndpoints(EDataFlow dataFlow, DeviceState stateMask, out IMMDeviceCollection devices);
        int GetDefaultAudioEndpoint(EDataFlow dataFlow, ERole role, out IMMDevice endpoint);
        int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string pwstrId, out IMMDevice device);
        int RegisterEndpointNotificationCallback(IntPtr client);
        int UnregisterEndpointNotificationCallback(IntPtr client);
    }

    [ComImport, Guid("0BD7A1BE-7A1A-44DB-8397-C0B87F2F409E"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IMMDeviceCollection {
        int GetCount(out uint pcDevices);
        int Item(uint nDevice, out IMMDevice device);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IMMDevice {
        int Activate(ref Guid iid, int dwClsCtx, IntPtr pActivationParams, [MarshalAs(UnmanagedType.IUnknown)] out object ppInterface);
        int OpenPropertyStore(int stgmAccess, out IPropertyStore properties);
        int GetId([MarshalAs(UnmanagedType.LPWStr)] out string ppstrId);
        int GetState(out DeviceState pdwState);
    }

    [ComImport, Guid("886d8eeb-8cf2-4446-8d02-cdba1dbdcf99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IPropertyStore {
        int GetCount(out uint cProps);
        int GetAt(uint iProp, out PROPERTYKEY pkey);
        int GetValue(ref PROPERTYKEY key, out PROPVARIANT pv);
        int SetValue(ref PROPERTYKEY key, ref PROPVARIANT propvar);
        int Commit();
    }

    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    public struct PROPERTYKEY {
        public Guid fmtid;
        public uint pid;
    }

    [StructLayout(LayoutKind.Explicit)]
    public struct PROPVARIANT {
        [FieldOffset(0)] public ushort vt;
        [FieldOffset(8)] public IntPtr pointerValue;
    }

    [ComImport, Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IAudioEndpointVolume {
        int RegisterControlChangeNotify(IntPtr pNotify);
        int UnregisterControlChangeNotify(IntPtr pNotify);
        int GetChannelCount(out uint pnChannelCount);
        int SetMasterVolumeLevel(float fLevelDB, Guid pguidEventContext);
        int SetMasterVolumeLevelScalar(float fLevel, Guid pguidEventContext);
        int GetMasterVolumeLevel(out float pfLevelDB);
        int GetMasterVolumeLevelScalar(out float pfLevel);
        int SetChannelVolumeLevel(uint nChannel, float fLevelDB, Guid pguidEventContext);
        int SetChannelVolumeLevelScalar(uint nChannel, float fLevel, Guid pguidEventContext);
        int GetChannelVolumeLevel(uint nChannel, out float pfLevelDB);
        int GetChannelVolumeLevelScalar(uint nChannel, out float pfLevel);
        int SetMute([MarshalAs(UnmanagedType.Bool)] bool bMute, Guid pguidEventContext);
        int GetMute([MarshalAs(UnmanagedType.Bool)] out bool pbMute);
        int GetVolumeStepInfo(out uint pnStep, out uint pnStepCount);
        int VolumeStepUp(Guid pguidEventContext);
        int VolumeStepDown(Guid pguidEventContext);
        int QueryHardwareSupport(out uint pdwHardwareSupportMask);
        int GetVolumeRange(out float pflVolumeMindB, out float pflVolumeMaxdB, out float pflVolumeIncrementdB);
    }

    [ComImport, Guid("f8679f50-850a-41cf-9c72-430f290290c8")]
    public class PolicyConfigClient { }

    [ComImport, Guid("f8679f50-850a-41cf-9c72-430f290290c8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    public interface IPolicyConfig {
        int GetMixFormat();
        int GetDeviceFormat();
        int ResetDeviceFormat();
        int SetDeviceFormat();
        int GetProcessingPeriod();
        int SetProcessingPeriod();
        int GetShareMode();
        int SetShareMode();
        int GetPropertyValue();
        int SetPropertyValue();
        int SetDefaultEndpoint([MarshalAs(UnmanagedType.LPWStr)] string wszDeviceId, ERole role);
        int SetEndpointVisibility();
    }

    public static class Native {
        public static readonly PROPERTYKEY PKEY_Device_FriendlyName = new PROPERTYKEY {
            fmtid = new Guid("a45c254e-df1c-4efd-8020-67d146a850e0"),
            pid = 14
        };

        public static string PropVariantToString(PROPVARIANT variant) {
            if (variant.vt == 31 && variant.pointerValue != IntPtr.Zero) {
                return Marshal.PtrToStringUni(variant.pointerValue);
            }
            return "";
        }

        [DllImport("ole32.dll")]
        public static extern int PropVariantClear(ref PROPVARIANT pvar);
    }
}
"@

function Get-AudioEnumerator {
    return [HyperXMicLite.IMMDeviceEnumerator]([HyperXMicLite.MMDeviceEnumeratorComObject]::new())
}

function Assert-HResult([int]$Result, [string]$Operation) {
    if ($Result -ne 0) {
        throw "$Operation failed with HRESULT 0x$($Result.ToString('X8'))."
    }
}

function Get-DeviceName($Device) {
    $store = $null
    Assert-HResult ($Device.OpenPropertyStore(0, [ref]$store)) "OpenPropertyStore"
    $key = [HyperXMicLite.Native]::PKEY_Device_FriendlyName
    $variant = New-Object HyperXMicLite.PROPVARIANT
    Assert-HResult ($store.GetValue([ref]$key, [ref]$variant)) "Get friendly name"
    try {
        return [HyperXMicLite.Native]::PropVariantToString($variant)
    }
    finally {
        [void][HyperXMicLite.Native]::PropVariantClear([ref]$variant)
    }
}

function Get-CaptureDevices {
    $enumerator = Get-AudioEnumerator
    $collection = $null
    Assert-HResult ($enumerator.EnumAudioEndpoints([HyperXMicLite.EDataFlow]::eCapture, [HyperXMicLite.DeviceState]::All, [ref]$collection)) "EnumAudioEndpoints"

    [uint32]$count = 0
    Assert-HResult ($collection.GetCount([ref]$count)) "GetCount"

    $devices = @()
    for ($i = 0; $i -lt $count; $i++) {
        $device = $null
        Assert-HResult ($collection.Item([uint32]$i, [ref]$device)) "Get device"

        $id = $null
        Assert-HResult ($device.GetId([ref]$id)) "Get device ID"
        $state = [HyperXMicLite.DeviceState]::Active
        Assert-HResult ($device.GetState([ref]$state)) "Get device state"

        $devices += [pscustomobject]@{
            Name = Get-DeviceName $device
            Id = $id
            State = $state
            Device = $device
        }
    }
    return $devices
}

function Get-DefaultCaptureDevice {
    $enumerator = Get-AudioEnumerator
    $device = $null
    Assert-HResult ($enumerator.GetDefaultAudioEndpoint([HyperXMicLite.EDataFlow]::eCapture, [HyperXMicLite.ERole]::eCommunications, [ref]$device)) "GetDefaultAudioEndpoint"
    return $device
}

function Get-EndpointVolume($Device) {
    $iid = [Guid]"5CDF2C82-841E-4546-9722-0CF74078229A"
    $obj = $null
    Assert-HResult ($Device.Activate([ref]$iid, 23, [IntPtr]::Zero, [ref]$obj)) "Activate IAudioEndpointVolume"
    return [HyperXMicLite.IAudioEndpointVolume]$obj
}

function Get-MicStatus {
    $device = Get-DefaultCaptureDevice
    $volume = Get-EndpointVolume $device
    [float]$level = 0
    [bool]$muted = $false
    Assert-HResult ($volume.GetMasterVolumeLevelScalar([ref]$level)) "Get volume"
    Assert-HResult ($volume.GetMute([ref]$muted)) "Get mute"

    $id = $null
    Assert-HResult ($device.GetId([ref]$id)) "Get device ID"

    return [pscustomobject]@{
        Name = Get-DeviceName $device
        Volume = [int][Math]::Round($level * 100)
        Muted = $muted
        Id = $id
    }
}

function Set-MicMute([bool]$Muted) {
    $volume = Get-EndpointVolume (Get-DefaultCaptureDevice)
    Assert-HResult ($volume.SetMute($Muted, [Guid]::Empty)) "Set mute"
}

function Set-MicVolume([int]$Percent) {
    if ($Percent -lt 0 -or $Percent -gt 100) {
        throw "Volume must be a number from 0 to 100."
    }
    $volume = Get-EndpointVolume (Get-DefaultCaptureDevice)
    Assert-HResult ($volume.SetMasterVolumeLevelScalar(($Percent / 100.0), [Guid]::Empty)) "Set volume"
}

function Set-DefaultCaptureDevice([string]$Selector) {
    if ([string]::IsNullOrWhiteSpace($Selector)) {
        throw "Pass part of the device name or the full endpoint ID."
    }

    $devices = Get-CaptureDevices | Where-Object { $_.State -eq [HyperXMicLite.DeviceState]::Active }
    $matches = @($devices | Where-Object { $_.Id -eq $Selector -or $_.Name -like "*$Selector*" })
    if ($matches.Count -eq 0) {
        throw "No active capture device matched '$Selector'. Run '.\HyperXMicLite.ps1 list'."
    }
    if ($matches.Count -gt 1) {
        $names = ($matches | ForEach-Object { $_.Name }) -join ", "
        throw "More than one device matched '$Selector': $names"
    }

    $policy = [HyperXMicLite.IPolicyConfig]([HyperXMicLite.PolicyConfigClient]::new())
    foreach ($role in @([HyperXMicLite.ERole]::eConsole, [HyperXMicLite.ERole]::eMultimedia, [HyperXMicLite.ERole]::eCommunications)) {
        Assert-HResult ($policy.SetDefaultEndpoint($matches[0].Id, $role)) "Set default endpoint"
    }
    return $matches[0]
}

function Show-DeviceList {
    $default = Get-MicStatus
    Get-CaptureDevices | ForEach-Object {
        [pscustomobject]@{
            Default = if ($_.Id -eq $default.Id) { "*" } else { "" }
            Name = $_.Name
            State = $_.State
            Id = $_.Id
        }
    } | Format-Table -AutoSize
}

function Show-Status {
    $status = Get-MicStatus
    "Default microphone: $($status.Name)"
    "Volume: $($status.Volume)%"
    "Muted: $($status.Muted)"
}

function Start-TrayApp {
    Add-Type -AssemblyName System.Windows.Forms
    Add-Type -AssemblyName System.Drawing
    [System.Windows.Forms.Application]::EnableVisualStyles()

    $menu = [System.Windows.Forms.ContextMenuStrip]::new()
    $statusItem = [System.Windows.Forms.ToolStripMenuItem]::new()
    $muteItem = [System.Windows.Forms.ToolStripMenuItem]::new("Toggle mute")
    $volumeMenu = [System.Windows.Forms.ToolStripMenuItem]::new("Set volume")
    $devicesMenu = [System.Windows.Forms.ToolStripMenuItem]::new("Default microphone")
    $refreshItem = [System.Windows.Forms.ToolStripMenuItem]::new("Refresh")
    $exitItem = [System.Windows.Forms.ToolStripMenuItem]::new("Exit")

    foreach ($percent in 100, 90, 80, 70, 60, 50, 40, 30, 20, 10) {
        $item = [System.Windows.Forms.ToolStripMenuItem]::new("$percent%")
        $captured = $percent
        $item.add_Click({ Set-MicVolume $captured; Update-Tray })
        [void]$volumeMenu.DropDownItems.Add($item)
    }

    $notify = [System.Windows.Forms.NotifyIcon]::new()
    $notify.Text = "HyperX Mic Lite"
    $notify.Icon = [System.Drawing.SystemIcons]::Information
    $notify.Visible = $true
    $notify.ContextMenuStrip = $menu

    function Update-Tray {
        $status = Get-MicStatus
        $statusItem.Text = "$($status.Name) - $($status.Volume)% - $(if ($status.Muted) { 'Muted' } else { 'Live' })"
        $statusItem.Enabled = $false
        $notify.Text = if ($status.Muted) { "Mic muted" } else { "Mic live at $($status.Volume)%" }

        $devicesMenu.DropDownItems.Clear()
        Get-CaptureDevices |
            Where-Object { $_.State -eq [HyperXMicLite.DeviceState]::Active } |
            ForEach-Object {
                $item = [System.Windows.Forms.ToolStripMenuItem]::new($_.Name)
                $item.Checked = $_.Id -eq $status.Id
                $deviceId = $_.Id
                $item.add_Click({ Set-DefaultCaptureDevice $deviceId | Out-Null; Update-Tray })
                [void]$devicesMenu.DropDownItems.Add($item)
            }
    }

    $muteItem.add_Click({
        $status = Get-MicStatus
        Set-MicMute (-not $status.Muted)
        Update-Tray
    })
    $refreshItem.add_Click({ Update-Tray })
    $exitItem.add_Click({
        $notify.Visible = $false
        $notify.Dispose()
        [System.Windows.Forms.Application]::Exit()
    })

    [void]$menu.Items.Add($statusItem)
    [void]$menu.Items.Add($muteItem)
    [void]$menu.Items.Add($volumeMenu)
    [void]$menu.Items.Add($devicesMenu)
    [void]$menu.Items.Add($refreshItem)
    [void]$menu.Items.Add($exitItem)

    Update-Tray
    [System.Windows.Forms.Application]::Run()
}

switch ($Command) {
    "list" { Show-DeviceList }
    "status" { Show-Status }
    "mute" { Set-MicMute $true; Show-Status }
    "unmute" { Set-MicMute $false; Show-Status }
    "toggle" {
        $status = Get-MicStatus
        Set-MicMute (-not $status.Muted)
        Show-Status
    }
    "volume" {
        if ([string]::IsNullOrWhiteSpace($Value)) { throw "Usage: .\HyperXMicLite.ps1 volume 75" }
        Set-MicVolume ([int]$Value)
        Show-Status
    }
    "default" {
        $device = Set-DefaultCaptureDevice $Value
        "Default microphone set to: $($device.Name)"
    }
    "tray" { Start-TrayApp }
}
