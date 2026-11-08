$ErrorActionPreference = "Stop"

$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root "build\gui-demo-app"
$clang = (Get-Command clang -ErrorAction Stop).Source
$ld = (Get-Command ld -ErrorAction Stop).Source
$objcopyCommand = Get-Command llvm-objcopy -ErrorAction SilentlyContinue
if (-not $objcopyCommand) {
    $objcopyCommand = Get-Command objcopy -ErrorAction Stop
}
$objcopy = $objcopyCommand.Source

New-Item -ItemType Directory -Path $build -Force | Out-Null
$compileFlags = @(
    "-target", "x86_64-unknown-none",
    "-ffreestanding",
    "-fno-stack-protector",
    "-fno-builtin",
    "-mno-red-zone",
    "-I", (Join-Path $root "userland")
)

& $clang @compileFlags -c (Join-Path $root "tests\fixtures\gui_demo_app.c") `
    -o (Join-Path $build "gui_demo_app.o")
if ($LASTEXITCODE -ne 0) { throw "Failed to compile GUI demo app." }

& $clang @compileFlags -c (Join-Path $root "userland\leafos_start.S") `
    -o (Join-Path $build "leafos_start.o")
if ($LASTEXITCODE -ne 0) { throw "Failed to assemble GUI demo startup." }

& $clang @compileFlags -c (Join-Path $root "userland\leafos_syscall.S") `
    -o (Join-Path $build "leafos_syscall.o")
if ($LASTEXITCODE -ne 0) { throw "Failed to assemble GUI demo syscalls." }

& $ld -T (Join-Path $root "userland\app.ld") `
    (Join-Path $build "leafos_start.o") `
    (Join-Path $build "gui_demo_app.o") `
    (Join-Path $build "leafos_syscall.o") `
    -o (Join-Path $build "gui_demo_app.elf")
if ($LASTEXITCODE -ne 0) { throw "Failed to link GUI demo app." }

& $objcopy --only-section=.text --only-section=.rodata -O binary `
    (Join-Path $build "gui_demo_app.elf") `
    (Join-Path $build "gui_demo_app.code")
if ($LASTEXITCODE -ne 0) { throw "Failed to extract GUI demo code." }

& $objcopy --only-section=.data -O binary `
    (Join-Path $build "gui_demo_app.elf") `
    (Join-Path $build "gui_demo_app.data")
if ($LASTEXITCODE -ne 0) { throw "Failed to extract GUI demo data." }

& (Join-Path $root "tools\pack-leaf-app.ps1") `
    -CodePath (Join-Path $build "gui_demo_app.code") `
    -DataPath (Join-Path $build "gui_demo_app.data") `
    -OutputPath (Join-Path $root "rootfs\apps\gui-demo.app")
if ($LASTEXITCODE -ne 0) { throw "Failed to package GUI demo app." }

Write-Output "Created rootfs\apps\gui-demo.app"
