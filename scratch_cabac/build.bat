@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
cd /d "D:\Programming\1PRODUCTION\Open Source\tpt-kinetix\scratch_cabac"
cl.exe /nologo harness.c
echo BUILD_EXIT=%ERRORLEVEL%
