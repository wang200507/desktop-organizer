# 内存基线测量脚本：构建 release 并实测工作集/私有内存
$ErrorActionPreference = 'Stop'
$root = 'D:\workBuddy\创意\desktop-organizer'
$cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
$exe = "$root\target\release\desktop-organizer.exe"

Write-Host "=== 1. cargo build --release ===" -ForegroundColor Cyan
& $cargo build --release --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { Write-Host "构建失败 exit=$LASTEXITCODE" -ForegroundColor Red; exit 1 }
Write-Host "构建成功，exe 大小: $([math]::Round((Get-Item $exe).Length/1KB,1)) KB" -ForegroundColor Green

Write-Host "`n=== 2. 启动 demo 并测内存 ===" -ForegroundColor Cyan
$p = Start-Process -FilePath $exe -PassThru
Start-Sleep -Seconds 3

$proc = Get-Process -Id $p.Id -ErrorAction SilentlyContinue
if ($null -eq $proc) { Write-Host "进程未运行（可能已退出）" -ForegroundColor Red; exit 1 }

$ws = [math]::Round($proc.WorkingSet64 / 1MB, 2)
$pm = [math]::Round($proc.PrivateMemorySize64 / 1MB, 2)
$pg = [math]::Round($proc.PagedMemorySize64 / 1MB, 2)

Write-Host "PID                = $($p.Id)"
Write-Host "WorkingSet (MB)    = $ws" -ForegroundColor Yellow
Write-Host "PrivateMemory (MB) = $pm" -ForegroundColor Yellow
Write-Host "PagedMemory (MB)   = $pg"

Write-Host "`n=== tasklist 确认 ===" -ForegroundColor Cyan
tasklist /fi "PID eq $($p.Id)" /fo csv /nh

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Write-Host "`n测量完成。目标：常驻 < 15 MB" -ForegroundColor Green
