@echo off
rem Double-click to install npp-acp into Notepad++ (will prompt for Administrator/UAC).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"
echo.
pause
