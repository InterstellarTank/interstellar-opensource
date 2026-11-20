# Lead live-login driver v2: clears browser focus stealers, forces focus on the game window,
# switches the game thread to EN layout, fills Email/Password and submits with Enter.
param(
    [string]$Email = "test@test.com",
    [string]$Password = "test123",
    [switch]$NoSubmit
)
$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class L2 {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct HARDWAREINPUT { public uint uMsg; public ushort wParamL; public ushort wParamH; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; [FieldOffset(0)] public HARDWAREINPUT hi; }
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public int type; public InputUnion U; }
  public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowTextW(IntPtr h, [MarshalAs(UnmanagedType.LPWStr)] StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern IntPtr LoadKeyboardLayoutW([MarshalAs(UnmanagedType.LPWStr)] string id, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr GetKeyboardLayout(uint tid);
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] inputs, int cbSize);
  [DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint code, uint map);
  public const int INPUT_KEYBOARD = 1;
  public const uint KEYEVENTF_KEYUP = 0x0002;

  public static List<string> ListWindows() {
    var list = new List<string>();
    EnumWindows((h, l) => {
      var sb = new StringBuilder(512);
      GetWindowTextW(h, sb, 512);
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (sb.Length > 0 && IsWindowVisible(h)) list.Add(pid + "|" + h.ToInt64() + "|" + sb.ToString());
      return true;
    }, IntPtr.Zero);
    return list;
  }
  public static uint Tid(IntPtr h) { uint pid; return GetWindowThreadProcessId(h, out pid); }
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

$VK_SHIFT=0x10; $VK_TAB=0x09; $VK_RETURN=0x0D; $VK_BACK=0x08
$dir = Join-Path $PSScriptRoot "..\.scratch\screenshots"

function Save-Shot([string]$path) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.X, $b.Y, 0, 0, $bmp.Size)
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
}

function Get-Game { $p = Get-Process WorldOfTanks -ErrorAction SilentlyContinue | Select-Object -First 1; if ($p) { $p.Refresh() }; return $p }

# ---- 1) close focus-stealing browser windows opened by the client's Registration link ----
$game = Get-Game
if (-not $game) { Write-Host "NO GAME PROCESS"; exit 2 }
$gamePid = $game.Id
foreach ($entry in [L2]::ListWindows()) {
    $parts = $entry.Split("|", 3)
    if ($parts.Count -lt 3) { continue }
    $pid2 = [int]$parts[0]; $h = [IntPtr][long]$parts[1]; $title = $parts[2]
    if ($pid2 -eq $gamePid) { continue }
    if ($title -match 'Account Creation|Wargaming|wargaming\.net|World of Tanks') {
        Write-Host "closing browser window pid=$pid2 title='$title'"
        [void][L2]::PostMessage($h, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)  # WM_CLOSE
    }
}
Start-Sleep -Milliseconds 800

# ---- 2) focus the game window (attach-thread-input trick, retried) ----
$hwnd = $game.MainWindowHandle
for ($i = 0; $i -lt 8; $i++) {
    [void][L2]::ShowWindow($hwnd, 9)
    [void][L2]::BringWindowToTop($hwnd)
    [void][L2]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 250
    if ([L2]::GetForegroundWindow() -eq $hwnd) { break }
    $fg = [L2]::GetForegroundWindow()
    $my = [L2]::GetCurrentThreadId(); $gt = [L2]::Tid($hwnd); $ft = [L2]::Tid($fg)
    [void][L2]::AttachThreadInput($my, $gt, $true)
    if ($fg -ne [IntPtr]::Zero) { [void][L2]::AttachThreadInput($my, $ft, $true) }
    [void][L2]::SetForegroundWindow($hwnd)
    [void][L2]::AttachThreadInput($my, $gt, $false)
    if ($fg -ne [IntPtr]::Zero) { [void][L2]::AttachThreadInput($my, $ft, $false) }
    Start-Sleep -Milliseconds 200
    if ([L2]::GetForegroundWindow() -eq $hwnd) { break }
}
$focused = ([L2]::GetForegroundWindow() -eq $hwnd)
Write-Host "game hwnd=$hwnd focused=$focused fg=$([L2]::GetForegroundWindow())"
if (-not $focused) { Save-Shot "$dir\lead2-focus-failed.png"; exit 3 }

# ---- 3) EN layout for the game thread ----
$hkl = [L2]::LoadKeyboardLayoutW("00000409", 1)
[void][L2]::PostMessage($hwnd, 0x0050, [IntPtr]::Zero, $hkl)
Start-Sleep -Milliseconds 500
Write-Host ("game HKL=0x{0:X8}" -f ([L2]::GetKeyboardLayout([L2]::Tid($hwnd))).ToInt64())

# ---- 4) type ----
function Tap([int]$vk) { [L2]::KeyVK([uint16]$vk, $false); Start-Sleep -Milliseconds 22; [L2]::KeyVK([uint16]$vk, $true); Start-Sleep -Milliseconds 28 }
function Backspace([int]$n) { for ($i=0; $i -lt $n; $i++) { Tap $VK_BACK }; Start-Sleep -Milliseconds 150 }
function TypeText([string]$s) {
    foreach ($ch in $s.ToCharArray()) {
        if ($ch -eq '@') { [L2]::KeyVK($VK_SHIFT,$false); Start-Sleep -Milliseconds 30; Tap 0x32; [L2]::KeyVK($VK_SHIFT,$true); Start-Sleep -Milliseconds 40 }
        elseif ($ch -eq '.') { Tap 0xBE }
        elseif ($ch -match '[a-zA-Z]') { Tap ([int][char]([string]$ch).ToUpper()) }
        elseif ($ch -match '[0-9]') { Tap (0x30 + [int]::Parse($ch)) }
        else { Write-Host "skip char $ch" }
    }
    Start-Sleep -Milliseconds 150
}

Backspace 40        # Email is auto-focused on a fresh login screen
TypeText $Email
Save-Shot "$dir\lead2-email-typed.png"
Tap $VK_TAB
Start-Sleep -Milliseconds 250
Backspace 25
TypeText $Password
Start-Sleep -Milliseconds 200
Save-Shot "$dir\lead2-fields-filled.png"
Write-Host "typed fields; screenshot saved"

if (-not $NoSubmit) {
    Tap $VK_RETURN
    Write-Host "pressed Enter"
    Start-Sleep -Seconds 3
    Save-Shot "$dir\lead2-after-enter-3s.png"
    Start-Sleep -Seconds 7
    Save-Shot "$dir\lead2-after-enter-10s.png"
}
Write-Host "done"
