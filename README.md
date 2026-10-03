# Useless Terminal (Rust)

A native Windows terminal emulator: tabs and split panes, a sessions sidebar, shell integration (OSC 7 / 133), SSH helpers,
file drops, logging and recording, 37 themes, Quake drop-down, tray, command palette. It is the full-Rust rewrite of the
WPF "Useless Terminal"; `RUST_TAURI_SPEC.md` is the behavioural specification (written for a Tauri/xterm.js build; the
stack decision since then is **egui + alacritty_terminal, no webview UI**).

Windows only (ConPTY). The executable is `UselessTerminal.exe`.

## Layout

| Crate | Role |
|---|---|
| `ut-fs` | atomic writes, debounced writer, app data folders (`UT_APPDATA` / `UT_LOCALAPPDATA` override them) |
| `ut-pty` | ConPTY: spawn, flow control, resize throttle, process tree policy, elevation |
| `ut-vt-scan` | OSC/BEL/alt-screen scanner, log stripper, asciicast writer |
| `ut-core` | settings, themes + presets, keybindings, window state, WPF migration |
| `ut-data` | sessions, snippets, workspaces, imports, environment layering |
| `ut-shell` | shell detection, per-shell integration injection, path quoting, git branch |
| `ut-ssh` | ssh discovery, quick connect, target parsing, connection reuse, file-drop transfer |
| `ut-term` | the terminal widget: alacritty grid + egui rendering, selection, search, links, Win32 key router |
| `ut-app` | the application (`UselessTerminal.exe`): window, tabs, sidebar, settings, palette, drops, Quake, tray |
| `legacy/` | the old Tauri/TypeScript code, kept only to port from; not part of the workspace |

## Build and test

```powershell
.\build.ps1                              # release exe -> dist\x64\UselessTerminal.exe and MSI -> dist\*.msi (needs WiX 5, see the header)
.\publish_github.ps1 v0.1.0 -WhatIf       # dry run; without -WhatIf it builds and publishes the exe (+ .sha256) as a GitHub release (needs gh)
cargo build -p useless-terminal          # package name, not the crate directory
cargo test --workspace
scripts\package.ps1 [-Arch x64|arm64]    # release exe + portable ZIP + MSI (WiX 5) + NSIS installer if makensis exists
bench\throughput.ps1                      # throughput / Ctrl+C-in-a-flood / keystroke latency of the release build
```

Use `CARGO_TARGET_DIR=<other dir>` when a test instance is still running from `target\debug` (Windows locks the exe).

## End-to-end scripts (`scripts\e2e-*.ps1`)

They drive the real window with `SendInput`, a real OLE drag, or posted window messages, against **isolated data folders**
(`%TEMP%\ut-e2e-<pid>`) and a copy of the exe. `UT_DUMP=<file>` makes the app write the active pane (stats + text) to a file
every 500 ms; `<file>.shot` saves a screenshot, `.toggle` does what the Quake hotkey does, `.quit` closes it.

`input` (typing, AltGr on es-ES, Ctrl+C), `shells` (cmd, PowerShell 5, Git Bash, WSL), `drop` (copy + conflict dialog),
`scroll` (ShareX WM_VSCROLL bridge), `quake`, `restore` (kill + relaunch), `clipboard`, `ui` (sidebar/settings, shortcuts while
a text field has focus), `browser` (the web panel against a local 127.0.0.1 server: lazy creation, bounds, airspace, address
bar, restart) and `browser-recover` (the page's window dies; switches reproduce what the full run does first).

Safety rules baked into `e2e-lib.ps1`: input is only sent while the app window is the foreground window; the app is always
stopped on error. Never point a test at the real `%APPDATA%\UselessTerminal` — the new code migrates the WPF files.

## Performance (i7-1165G7, Windows 11, release build, `bench\throughput.ps1`)

| | Result | Budget |
|---|---|---|
| flood, producer writes big blocks (`copy /b file CON`) | 12.1 MB/s | ≥ 10 MB/s |
| Ctrl+C: output stops (line writer / block writer) | 117 ms / 401 ms | ≤ 300 ms |
| keystroke → echo, clean `pwsh` (p50 / p99, ±20 ms sampling) | 42 ms / 103 ms | p99 ≤ 100 ms |
| flood, producer writes line by line (`type`, `wsl seq`) | 0.4–1 MB/s | ≥ 10 MB/s |

The last row is **not the terminal**: a ConPTY-only test (`ut-pty`, `perf_type_of_a_big_file`, no UI, a sink that only counts)
gets the same 0.5–1 MB/s, with either the inbox conhost or a bundled OpenConsole `conpty.dll`, because conhost handles
roughly 12k console writes per second here (`cmd /c type` writes each 79-byte line separately). The terminal's own
parse + grid stage (`ut-term`, `perf_sink_throughput`) does 12–22 MB/s. Windows Terminal was not available to compare.
After Ctrl+C the block writer keeps delivering what conhost had already buffered (up to ~400 ms).

## Things to know

- **`UselessTerminal.exe` is a single self-contained file** (`.cargo\config.toml` links the C runtime statically: it imports only
  Windows system DLLs, no Visual C++ Redistributable, no sidecar files). It still relies on what Windows provides: ConPTY
  (Windows 10 1809+), a GPU/DirectX stack for wgpu, and, for the browser panel only, the WebView2 runtime (preinstalled on
  Windows 11; the panel reports an error without it and everything else keeps working).
- **Installing the MSI replaces the WPF version**: it deliberately shares the WPF MSI's UpgradeCode.
- No inline images (sixel / iTerm): `alacritty_terminal` has none. Emoji render monochrome in egui.
- The browser panel (spec §17) is the only web engine in the app: a lazily created, isolated child WebView2 behind the
  `browser-panel` cargo feature.
- Windows 11 Mica/Acrylic (`ui.backdrop`) is exposed in settings but not implemented (it needs a transparent wgpu surface).
  CRT mode has scanlines, vignette and flicker but no text glow.
- The NSIS installer script (`installer\UselessTerminal.nsi`) has never been built here (`makensis` is not installed).
- Build output is large (several GB per target dir) and this repo lives under OneDrive: point `CARGO_TARGET_DIR` outside it.

## Developer

**Unnamed10110**

- trojan.v6@gmail.com
- sergiobritos10110@gmail.com
