@echo off
rem Backup the source tree before making changes.
rem Usage:  backup.cmd "what you are about to do"
setlocal
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0backup.ps1" -Desc "%~1"
endlocal
