@echo off
REM Local build script (ASCII only: cmd.exe parses .bat using the OEM codepage).
REM
REM Toolchain notes:
REM   - No MSVC build tools and no MinGW gcc on this machine.
REM   - With a windows-gnu *host*, windows-link emits `raw-dylib` imports and
REM     rustc shells out to Cygwin dlltool, which cannot start inside the DSH
REM     sandbox (it needs a named pipe under %TEMP%).
REM   - Workaround: keep the pre-built proc-macros cached (do NOT wipe target/)
REM     and build the final artifact for x86_64-pc-windows-gnullvm, whose std is
REM     self-contained and never invokes dlltool.
REM   - The system %TEMP% is sandboxed, so TMP/TEMP point at the workspace.
REM
REM Usage:
REM   build.cmd                       -> debug build
REM   build.cmd test                  -> run tests
REM   build.cmd run -p mcworld-app    -> run the GUI
REM   build.cmd build --release ...   -> only if proc-macros are already cached
setlocal
set ROOT=%~dp0
set TC=%ROOT%.toolchain
set RUSTUP_HOME=%TC%\rustup
set CARGO_HOME=%TC%\cargo
set PATH=%TC%\cargo\bin;%PATH%
set TMP=%ROOT%.build\tmp
set TEMP=%ROOT%.build\tmp
if not exist "%TMP%" mkdir "%TMP%"
set RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu
set CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER=%TC%\rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\rust-lld.exe

pushd "%ROOT%"
REM Local linker flags live in .cargo/config.local.toml (not in git).
REM Use --config to layer them on top of the repo .cargo/config.toml.
set LOCALCFG=
if exist "%ROOT%.cargo\config.local.toml" set LOCALCFG=--config "%ROOT%.cargo\config.local.toml"

REM Regenerate the resource object (icon + version info + manifest) from
REM the ICO and the version in Cargo.toml, so VERSIONINFO never goes stale.
if exist "%ROOT%.ci\make_rsrc_obj.js" (
    node "%ROOT%.ci\make_rsrc_obj.js" "%ROOT%crates\app\assets\icon.ico" "%ROOT%crates\app\assets\icon_obj.obj"
)

if "%~1"=="" (
    cargo build %LOCALCFG% --target x86_64-pc-windows-gnullvm
) else (
    cargo %* %LOCALCFG% --target x86_64-pc-windows-gnullvm
)
set RC=%ERRORLEVEL%
if %RC%==0 (
    REM Resource check: the icon/version/manifest object now carries real
    REM ADDR32NB relocations, so the linker fills in the correct RVAs and
    REM no post-link patching is needed. Just verify they are all present.
    if exist "%ROOT%.ci\check_resources.js" (
        node "%ROOT%.ci\check_resources.js" "%ROOT%target\x86_64-pc-windows-gnullvm\debug\mcworld-app.exe"
    )
)
popd
endlocal & exit /b %RC%
