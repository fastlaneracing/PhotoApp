@echo off
:: Build photoApp into one standalone exe:
::   src-tauri\target\release\photoapp.exe
:: Double-click that exe to run the app. Vite and the API server are not needed.
:: Full output is also saved to build.log (send it over if a build fails).
setlocal
cd /d "%~dp0"

:: Windows locks a running exe, so the build cannot replace it.
tasklist /FI "IMAGENAME eq photoapp.exe" 2>nul | find /I "photoapp.exe" >nul
if not errorlevel 1 (
  echo.
  echo photoApp is still running. Close it ^(check the taskbar / system tray^),
  echo then run build.bat again.
  echo.
  pause
  exit /b 1
)

echo Build started %date% %time% > build.log

echo [1/2] Building frontend (frontend\dist)...
pushd frontend
if not exist node_modules call npm install >> ..\build.log 2>&1 || goto :fail
call npm run build >> ..\build.log 2>&1 || goto :fail
popd

echo [2/2] Building release exe (frontend embedded) - first build after an update can take several minutes...
pushd src-tauri
cargo build --release >> ..\build.log 2>&1 || goto :fail
popd

echo.
echo Done: %~dp0src-tauri\target\release\photoapp.exe
echo Build finished %date% %time% >> build.log
pause
exit /b 0

:fail
popd 2>nul
echo.
echo BUILD FAILED. Last lines of build.log:
echo ------------------------------------------------------------
powershell -NoProfile -Command "Get-Content -Tail 30 '%~dp0build.log'"
echo ------------------------------------------------------------
echo Full log: %~dp0build.log
pause
exit /b 1
