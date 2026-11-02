# Builds reel on Windows without Visual Studio, using MSYS2's clang as the linker.
param([string[]]$CargoArgs = @("build", "--release"))
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\msys64\clang64\bin;$env:Path"
cargo +stable-x86_64-pc-windows-gnullvm @CargoArgs
