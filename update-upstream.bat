@echo off
rem Pulls the upstream creator's newest release into the FigIran app (this folder): rebrand + merge + cargo check; -Push also pushes the app repo.
rem Usage: update-upstream.bat [-Push] [-NoCheck] [-Ref upstream/release]
pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0figiran-tools\rebrand\update-from-upstream.ps1" %*
set "RC=%ERRORLEVEL%"
echo.
echo Exit code: %RC%
pause
exit /b %RC%
