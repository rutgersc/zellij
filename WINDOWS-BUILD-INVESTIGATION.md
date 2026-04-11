# Windows Build Investigation: libz-sys / cl.exe failure

## Error

```
error occurred in cc-rs: command did not execute successfully (status code exit code: 2):
"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.41.34120\bin\HostX64\x64\cl.exe"
"-nologo" "-MD" "-O2" "-Brepro" "-I" "src/zlib" "-W0" "-DSTDC"
"-FoF:\GitRepos\zellij\target\release\build\libz-sys-f7926dd8b044d9fa\out\lib\f0389296f42960e9-gzwrite.o"
"-c" "src/zlib/gzwrite.c"
```

## Root cause

`cl.exe` is on PATH and executes, but the `INCLUDE` environment variable is empty. Without it, `cl.exe` cannot find standard C headers (`stdio.h`, etc.) and fails with exit code 2.

**Evidence:**
- `cl.exe` runs and reports version 19.41.34123 for x64 (verified by running `cl.exe` directly)
- `printenv INCLUDE` returns nothing — the MSVC environment was never initialized
- The standard fix is to run from "x64 Native Tools Command Prompt" which calls `vcvarsall.bat` to set `INCLUDE`, `LIB`, `LIBPATH`

## Dependency chain

Source: `cargo tree -i libz-sys`

```
libz-sys v1.1.8
├── curl-sys v0.4.68+curl-8.4.0
│   ├── curl v0.4.44
│   │   └── isahc v1.7.2
│   │       ├── zellij
│   │       ├── zellij-client
│   │       ├── zellij-server
│   │       └── zellij-utils
│   ├── isahc v1.7.2
│   └── zellij-utils
└── libssh2-sys v0.2.23
    └── ssh2 v0.9.3 (dev-dependency of zellij)
```

## Still to research

- [ ] Locate `vcvarsall.bat` on this system (expected path `C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvarsall.bat` was not found — may be a different VS edition or install path)
- [ ] Extract the exact values of `INCLUDE`, `LIB`, `LIBPATH` that vcvarsall sets for x64
- [ ] Determine which of these are actually required for `cc-rs` / `cl.exe` to compile C code
- [ ] Decide: set permanently in system env vars, or set in shell profile (e.g. `.bashrc`), or use a wrapper script
- [ ] Alternative: can `libz-sys` use a prebuilt zlib (e.g. via `vcpkg` or `LIBZ_SYS_STATIC`) to skip C compilation entirely?
