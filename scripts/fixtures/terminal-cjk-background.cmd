@echo off
rem Rendering fixture for scripts/ui-screenshot.ps1 -Shell: CJK wide chars on plain,
rem colored-background and inverse (selection-like) rows. File is UTF-8 without BOM.
chcp 65001 >nul
echo plain    : 请分析定位当前项目 NRF性能目标 ok
echo [48;5;238mbackground: 请分析定位当前项目 NRF性能目标 ok[0m
echo [7minverse   : 请分析定位当前项目 NRF性能目标 ok[0m
echo [44;97mmixed     : 宽字符ab中文c[0m
pause >nul
