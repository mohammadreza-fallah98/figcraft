@echo off
rem Builds the FigIran desktop app (with portal login) into release\FigIran.exe next to this file
rem Usage: build-app.bat [portal-url]    (default: http://127.0.0.1:4000)
rem Needs: Rust (cargo), Visual Studio MSVC tools, and the Windows SDK libs from xwin in %USERPROFILE%\xwin\splat

setlocal
set "ROOT=%~dp0"
set "MSVC=C:\Program Files\Microsoft Visual Studio\18\Enterprise\VC\Tools\MSVC\14.50.35717"
set "XWIN=%USERPROFILE%\xwin\splat"

set "PATH=%MSVC%\bin\Hostx64\x64;%USERPROFILE%\.cargo\bin;%PATH%"
set "LIB=%MSVC%\lib\x64;%XWIN%\crt\lib\x86_64;%XWIN%\sdk\lib\um\x86_64;%XWIN%\sdk\lib\ucrt\x86_64"
set "INCLUDE=%XWIN%\crt\include;%XWIN%\sdk\include\ucrt;%XWIN%\sdk\include\um;%XWIN%\sdk\include\shared"
set "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=%MSVC%\bin\Hostx64\x64\link.exe"

rem Build cache (about 8 GB, git-ignored) lives in build-cache; release only receives the final FigIran.exe
set "CACHE=%ROOT%build-cache"
set "CARGO_TARGET_DIR=%CACHE%"

set "FIGIRAN_PORTAL_URL=%~1"
if "%FIGIRAN_PORTAL_URL%"=="" set "FIGIRAN_PORTAL_URL=http://127.0.0.1:4000"

if not exist "%ROOT%Cargo.toml" (
  echo The app source is not at %ROOT%
  pause
  exit /b 1
)
if not exist "%MSVC%\bin\Hostx64\x64\link.exe" (
  echo MSVC link.exe not found: %MSVC%
  pause
  exit /b 1
)
if not exist "%XWIN%\sdk\lib\um\x86_64" (
  echo Windows SDK libs not found in %XWIN%
  pause
  exit /b 1
)

rem A running copy locks the exe
taskkill /F /IM FigIran.exe >nul 2>&1

echo Portal URL baked into the app: %FIGIRAN_PORTAL_URL%
pushd "%ROOT%"
cargo build --release -p figiran
set "RC=%ERRORLEVEL%"
popd

if not "%RC%"=="0" (
  echo Build failed with code %RC%
  pause
  exit /b %RC%
)

rem cargo names the binary figiran.exe; the shipped file is release\FigIran.exe
if not exist "%ROOT%release" mkdir "%ROOT%release"
copy /Y "%CACHE%\release\figiran.exe" "%ROOT%release\FigIran.exe" >nul

rem winresource needs rc.exe (Windows SDK), which is not installed here, so the icon is stamped afterwards with
rem rcedit (npm package, installed into %TEMP% on first use; it bundles rcedit-x64.exe).
set "RCEDIT_DIR=%TEMP%\figiran-rcedit"
set "RCEDIT=%RCEDIT_DIR%\node_modules\rcedit\bin\rcedit-x64.exe"
if not exist "%RCEDIT%" call npm install --no-save --no-audit --no-fund --prefix "%RCEDIT_DIR%" rcedit@5.0.2
echo Stamping the FigIran icon into the exe
"%RCEDIT%" "%ROOT%release\FigIran.exe" --set-icon "%ROOT%assets\app-icon\figiran.ico" --set-version-string ProductName FigIran --set-version-string FileDescription FigIran --set-version-string CompanyName FigIran --set-version-string OriginalFilename FigIran.exe
if errorlevel 1 (
  echo rcedit failed: the exe has no custom icon
  pause
  exit /b 1
)

echo.
echo Done: %ROOT%release\FigIran.exe
pause
endlocal
