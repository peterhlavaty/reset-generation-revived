@echo off
rem Reset Generation Revived: lets this PC host LAN games (Windows Firewall, port 8787).
rem Double-click it and confirm the administrator prompt. "LAN-Setup.cmd -Remove" undoes it.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0app\lan-setup.ps1" %*
