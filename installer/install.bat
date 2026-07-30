@echo off
setlocal

set "APP_NAME=Voice Keys"
set "BIN_NAME=voicekeys.exe"
set "INSTALL_DIR=%LOCALAPPDATA%\VoiceKeys"

echo.
echo  Installing %APP_NAME%...
echo.

:: Create install directory
if not exist "%INSTALL_DIR%" mkdir "%INSTALL_DIR%"

:: Copy exe
copy /Y "%~dp0%BIN_NAME%" "%INSTALL_DIR%\%BIN_NAME%" >nul
if errorlevel 1 (
    echo  ERROR: Failed to copy %BIN_NAME%. Is the app currently running?
    echo  Close Voice Keys and try again.
    pause
    exit /b 1
)

:: Copy icon if present
if exist "%~dp0icon.ico" copy /Y "%~dp0icon.ico" "%INSTALL_DIR%\icon.ico" >nul

:: Create Start Menu shortcut
set "SHORTCUT=%APPDATA%\Microsoft\Windows\Start Menu\Programs\%APP_NAME%.lnk"
powershell -NoProfile -Command ^
  "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('%SHORTCUT%'); $s.TargetPath = '%INSTALL_DIR%\%BIN_NAME%'; $s.WorkingDirectory = '%INSTALL_DIR%'; $s.Description = 'Voice Keys - push-to-talk transcription'; if (Test-Path '%INSTALL_DIR%\icon.ico') { $s.IconLocation = '%INSTALL_DIR%\icon.ico' }; $s.Save()"

echo  Installed to: %INSTALL_DIR%
echo  Start Menu shortcut created.
echo.

:: Ask about startup
set /p STARTUP="  Start Voice Keys automatically when you log in? (Y/n): "
if /i "%STARTUP%"=="n" goto :skip_startup

:: Create startup shortcut
set "STARTUP_LNK=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\%APP_NAME%.lnk"
powershell -NoProfile -Command ^
  "$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('%STARTUP_LNK%'); $s.TargetPath = '%INSTALL_DIR%\%BIN_NAME%'; $s.WorkingDirectory = '%INSTALL_DIR%'; $s.Description = 'Voice Keys - push-to-talk transcription'; if (Test-Path '%INSTALL_DIR%\icon.ico') { $s.IconLocation = '%INSTALL_DIR%\icon.ico' }; $s.Save()"
echo  Added to startup.

:skip_startup
echo.

:: Launch the app
echo  Launching %APP_NAME%...
start "" "%INSTALL_DIR%\%BIN_NAME%"

echo.
echo  Done! You can close this window.
echo.
pause
