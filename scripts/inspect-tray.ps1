# Read-only Explorer window inventory for tray compatibility diagnostics.
param([switch]$Capture, [switch]$TestHiddenCapture)
Add-Type -ReferencedAssemblies System.Drawing @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class TrayInventory {
    public delegate bool Callback(IntPtr hwnd, IntPtr data);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, Callback callback, IntPtr data);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr hwnd, StringBuilder text, int count);
    [DllImport("user32.dll")] public static extern IntPtr GetParent(IntPtr hwnd);
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr dc, uint flags);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    public static void Capture(string path) {
        var h = FindWindow("Shell_TrayWnd", null);
        Rect r; if (h == IntPtr.Zero || !GetWindowRect(h, out r)) return;
        using (var bitmap = new System.Drawing.Bitmap(r.Right-r.Left, r.Bottom-r.Top)) {
            using (var graphics = System.Drawing.Graphics.FromImage(bitmap)) {
                var dc = graphics.GetHdc();
                try { Console.WriteLine("PrintWindow: " + PrintWindow(h, dc, 2)); }
                finally { graphics.ReleaseHdc(dc); }
            }
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
    }
    public static void Dump() {
        var root = FindWindow("Shell_TrayWnd", null);
        Console.WriteLine("Explorer taskbar: " + root);
        EnumChildWindows(root, (h, d) => {
            var text = new StringBuilder(256);
            GetClassName(h, text, text.Capacity);
            Console.WriteLine(h + " parent=" + GetParent(h) + " " + text);
            return true;
        }, IntPtr.Zero);
    }
}
'@
[TrayInventory]::Dump()
if ($Capture) { [TrayInventory]::Capture((Join-Path $PSScriptRoot '../target/tray-capture.png')) }
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$trayHandle = [TrayInventory]::FindWindow('Shell_TrayWnd', $null)
if ($trayHandle -ne [IntPtr]::Zero) {
    $wasVisible = [TrayInventory]::IsWindowVisible($trayHandle)
    $trayElement = [System.Windows.Automation.AutomationElement]::FromHandle($trayHandle)
    $cachedButtons = $trayElement.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    try {
    if ($TestHiddenCapture) {
        [void][TrayInventory]::ShowWindow($trayHandle, 0)
        [TrayInventory]::Capture((Join-Path $PSScriptRoot '../target/tray-hidden-capture.png'))
    }
    $trayElement = [System.Windows.Automation.AutomationElement]::FromHandle($trayHandle)
    $buttons = if ($TestHiddenCapture) { $cachedButtons } else { $trayElement.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) }
    foreach ($button in $buttons) {
        $info = $button.Current
        if ($info.ClassName -like '*Tray*' -or $info.AutomationId -like '*Tray*') {
            [PSCustomObject]@{Class=$info.ClassName; Id=$info.AutomationId; Name=$info.Name; Bounds=$info.BoundingRectangle.ToString(); Patterns=($button.GetSupportedPatterns().ProgrammaticName -join ',')}
        }
    }
    } finally {
        if ($TestHiddenCapture -and $wasVisible) { [void][TrayInventory]::ShowWindow($trayHandle, 8) }
    }
}
