param(
    [string]$NppDir = "C:\Program Files\Notepad++",
    [switch]$Uninstall
)
$ErrorActionPreference = "Stop"

$root = Split-Path $PSScriptRoot -Parent
$pluginDst = Join-Path $NppDir "plugins\npp-acp"

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Warning "需要管理员权限写入 $NppDir，正在请求提权…"
    $args = "-NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`" -NppDir `"$NppDir`"" + $(if ($Uninstall) { " -Uninstall" } else { "" })
    Start-Process powershell -Verb RunAs -ArgumentList $args
    return
}

if ($Uninstall) {
    if (Test-Path $pluginDst) { Remove-Item $pluginDst -Recurse -Force; Write-Host "已卸载: $pluginDst" }
    return
}

$src = Join-Path $root "dist\npp-acp"
if (-not (Test-Path $src)) { throw "找不到 $src，请先运行 scripts\build.ps1" }
$plugins = Join-Path $NppDir "plugins"
if (-not (Test-Path $plugins)) { throw "找不到 Notepad++ 插件目录: $plugins" }

if (Test-Path $pluginDst) { Remove-Item $pluginDst -Recurse -Force }
Copy-Item $src $pluginDst -Recurse -Force

Write-Host "已安装到: $pluginDst"
Write-Host "重启 Notepad++ → 菜单『插件』→『Agent ACP』打开右侧面板。"
Write-Host "提示：默认 agent 命令为 claude-agent-acp.cmd；可用环境变量 NPP_ACP_AGENT 覆盖。"
