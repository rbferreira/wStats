@echo off
rem Builds the two DLLs that run inside explorer.exe, plus the dev injector.
setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul 2>nul
if not exist out mkdir out
set CFLAGS=/nologo /std:c++20 /EHsc /O2 /MT /W4 /permissive- /DWIN32_LEAN_AND_MEAN /DNOMINMAX /Zc:__cplusplus
cl %CFLAGS% tap.cpp /LD /Fe:out\wStatsTAP.dll /Fo:out\ /link /DEF:tap.def windowsapp.lib ole32.lib user32.lib || exit /b 1
cl %CFLAGS% core.cpp /LD /Fe:out\wStatsCore.dll /Fo:out\ /link /DEF:core.def windowsapp.lib ole32.lib user32.lib advapi32.lib || exit /b 1
cl %CFLAGS% inject.cpp /Fe:out\inject.exe /Fo:out\ /link user32.lib || exit /b 1
