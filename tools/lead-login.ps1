# Lead's live login driver for the WoT 0.6.5 client.
# Run with: powershell -NoProfile -ExecutionPolicy Bypass -File tools\lead-login.ps1 [-Email ...] [-Password ...]
param(
    [string]$Email = "test@test.com",
    [string]$Password = "test123",
    [switch]$NoSubmit,
    [string]$OutDir = (Join-Path $PSScriptRoot "..\.scratch\screenshots")
)
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

if (-not ("LeadInput" -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class LeadInput {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X,Y; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct HARDWAREINPUT { public uint uMsg; public ushort wParamL; public ushort wParamH; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; [FieldOffset(0)] public HARDWAREINPUT hi; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public int type; public InputUnion U; }
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern IntPtr LoadKeyboardLayoutW([MarshalAs(UnmanagedType.LPWStr)] string id, uint flags);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int cbSize);
  [DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint code, uint map);
  public const uint KEYEVENTF_KEYUP = 0x0002;
  public const int INPUT_KEYBOARD = 1;
  public static void KeyVK(ushort vk, bool up) {
    INPUT[] inp = new INPUT[1];
    inp[0].type = INPUT_KEYBOARD;
    inp[0].U.ki = new KEYBDINPUT();
    inp[0].U.ki.wVk = vk;
    inp[0].U.ki.wScan = (ushort)MapVirtualKeyW(vk, 0);
    inp[0].U.ki.dwFlags = up ? KEYEVENTF_KEYUP : 0;
    SendInput(1, inp, Marshal.SizeOf(typeof(INPUT)));
  }
}
'@
}

$VK_SHIFT = 0x10; $VK_TAB = 0x09; $VK_RETURN = 0x0D

function Get-GameWindow {
    $p = Get-Process WorldOfTanks -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $p) { return $null }
    $p.Refresh()
    return $p.MainWindowHandle
}

function Save-Shot([string]$path) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.X, $b.Y, 0, 0, $bmp.Size)
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

function Send-Char([char]$c) {
    if ($c -eq '@') {
        [LeadInput]::KeyVK($VK_SHIFT, $false); Start-Sleep -Milliseconds 25
        [LeadInput]::KeyVK(0x32, $false); Start-Sleep -Milliseconds 25
        [LeadInput]::KeyVK(0x32, $true); Start-Sleep -Milliseconds 25
        [LeadInput]::KeyVK($VK_SHIFT, $true); Start-Sleep -Milliseconds 25
    } elseif ($c -eq '.') {
        [LeadInput]::KeyVK(0xBE, $false); Start-Sleep -Milliseconds 25
        [LeadInput]::KeyVK(0xBE, $true); Start-Sleep -Milliseconds 25
    } elseif ($c -match '[a-zA-Z]') {
        $vk = [int][char]([string]$c).ToUpper()
        [LeadInput]::KeyVK([uint16]$vk, $false); Start-Sleep -Milliseconds 25
        [LeadInput]::KeyVK([uint16]$vk, $true); Start-Sleep -Milliseconds 25
    } elseif ($c -match '[0-9]') {
        $vk = 0x30 + [int]::Parse($c)
        [LeadInput]::KeyVK([uint16]$vk, $false); Start-Sleep -Milliseconds 25
        [LeadInput]::KeyVK([uint16]$vk, $true); Start-Sleep -Milliseconds 25
    } else {
        Write-Host "  (skipping unsupported char '$c')"
    }
}

$hwnd = Get-GameWindow
if (-not $hwnd -or $hwnd -eq [IntPtr]::Zero) { Write-Host "NO GAME WINDOW"; exit 2 }
[void][LeadInput]::BringWindowToTop($hwnd)
[void][LeadInput]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 400
$fg = [LeadInput]::GetForegroundWindow()
Write-Host "hwnd=$hwnd foreground=$fg same=$($fg -eq $hwnd)"
Save-Shot (Join-Path $OutDir "lead-02-before-typing.png")

# switch the game thread's keyboard layout to US English so scancodes yield ASCII
$hkl = [LeadInput]::LoadKeyboardLayoutW("00000409", 1)  # KLF_ACTIVATE
[void][LeadInput]::PostMessage($hwnd, 0x0050, [IntPtr]::Zero, $hkl)  # WM_INPUTLANGCHANGEREQUEST
Start-Sleep -Milliseconds 500

# Email field is auto-focused on a fresh login screen.
foreach ($ch in $Email.ToCharArray()) { Send-Char $ch }
Start-Sleep -Milliseconds 200
[LeadInput]::KeyVK($VK_TAB, $false); Start-Sleep -Milliseconds 40; [LeadInput]::KeyVK($VK_TAB, $true)
Start-Sleep -Milliseconds 300
foreach ($ch in $Password.ToCharArray()) { Send-Char $ch }
Start-Sleep -Milliseconds 300
Save-Shot (Join-Path $OutDir "lead-03-filled.png")
Write-Host "typed email/password; filled screenshot saved"

if (-not $NoSubmit) {
    [LeadInput]::KeyVK($VK_RETURN, $false); Start-Sleep -Milliseconds 50; [LeadInput]::KeyVK($VK_RETURN, $true)
    Write-Host "pressed Enter"
}
Start-Sleep -Seconds 3
Save-Shot (Join-Path $OutDir "lead-04-after-submit-3s.png")
Start-Sleep -Seconds 7
Save-Shot (Join-Path $OutDir "lead-05-after-submit-10s.png")
Write-Host "done"
