@echo off
rem Builds the FigIran web build (static site) into dist\web, with the portal address baked in so the page can sign in.
rem Usage: build-web.bat [portal-url]    (default: http://127.0.0.1:4000)
setlocal
set "PORTAL=%~1"
if "%PORTAL%"=="" set "PORTAL=http://127.0.0.1:4000"
pwsh -NoProfile -ExecutionPolicy Bypass -File "%~dp0figiran-tools\build-web.ps1" -PortalUrl "%PORTAL%"
set "RC=%ERRORLEVEL%"
echo.
if "%RC%"=="0" (echo Web build done.) else (echo Web build FAILED with code %RC%.)
pause
exit /b %RC%
