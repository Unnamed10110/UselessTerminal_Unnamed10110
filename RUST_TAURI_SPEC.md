# Useless Terminal — Rust + Tauri Reimplementation Specification

**Spec version:** 1.0 (2026-10-01)
**Describes:** the WPF/.NET 9 app in this repository (commit `ad660df` plus the WiX MSI working-tree changes), and the target behavior for a from-scratch rewrite in **Rust + Tauri v2**.
**Platform:** Windows only for v1 (the architecture keeps the PTY behind a trait so Unix can be added later).

---

## 0. How to use this document

- **Audience:** an engineer or model implementing the app from scratch, with no access to the original code.
- **Keywords:** MUST / SHOULD / MAY are used in the RFC 2119 sense.
- **Priorities:**
  - **[P0]** — needed for parity with the current app. Required for v1.
  - **[P1]** — strongly desired improvement. Ship in v1 if at all possible.
  - **[P2]** — later.
- **"WPF:" notes** describe the current implementation, for context only. The current implementation has bugs; §24 lists them. **Do not replicate anything in §24.**
- **Conflicts:** when this spec and the current app disagree, this spec wins. When this spec is silent on a detail, match the current behavior described here.
- **"(verify)"** marks an assumption about a third-party API or version. Confirm it against current documentation before building on it.
- §23 lists hard-won compatibility lessons from the current app. Every item there exists because something broke in practice. Keep them all.

---

## 1. Product summary

Useless Terminal is a fast Windows terminal emulator with:

- **Terminal core:** a ConPTY backend and a GPU-accelerated xterm.js renderer.
- **Layout:** tabs, plus up to 4 split panes per tab ([P1]: an arbitrary split tree).
- **Sessions sidebar:** inspired by MobaXterm. Saved sessions in folders, with per-session color, environment, theme overrides, and a starting command. Also holds command snippets.
- **Shell integration:** OSC 133 prompt and command markers, OSC 7 current directory. Exit codes show in the status bar, and you can jump between commands.
- **SSH conveniences:** quick connect, import from `~/.ssh/config`, colors that work over SSH, and drag-and-drop file upload into the remote current directory.
- **Built-in browser panel:** for AI chat sites, with its own isolated browser profile.
- **Window extras:** Quake-style global hotkey, tray icon, command palette, workspaces, logging, asciicast recording, CRT effect, minimap, and 31 theme presets.

**Non-goals for v1:**

- macOS or Linux builds.
- Writing our own VT parser or renderer (use xterm.js).
- Acting as the Windows 11 "default terminal application" (the ITerminalHandoff COM handoff).
- Telemetry of any kind.
- Storing SSH passwords.

---

## 2. Compatibility targets

### 2.1 Operating system and runtime
- **[P0]** Windows 10 1809 (build 17763) and later, x64 and ARM64. This minimum comes from ConPTY.
- **[P0]** Recommended: Windows 10 21H2+ or Windows 11. These have much better ConPTY behavior.
- **[P0]** WebView2 Evergreen Runtime. Detect it at startup; if it is missing, show an actionable error with a download link.
- **[P1]** Optionally bundle a newer `conpty.dll` + `OpenConsole.exe` (the Microsoft.Windows.Console.ConPTY package, the same thing Windows Terminal and VS Code ship). Fall back to the built-in `kernel32` ConPTY. This fixes many rendering, reflow and resize bugs in older built-in conhost versions. Setting: `terminal.conptyImplementation = "auto" | "bundled" | "system"` (verify the package name, license and redistribution terms).

### 2.2 Shells that MUST work [P0]
- PowerShell 7+ (`pwsh.exe`).
- Windows PowerShell 5.1.
- `cmd.exe`.
- WSL: generic, plus each distro.
- Git Bash.
- `ssh.exe`: the built-in Win32-OpenSSH, or Git for Windows' copy.
- Any arbitrary executable with arguments.
- **[P2]** MSYS2, Cygwin, Nushell, Visual Studio Developer shells (via `vswhere`).

### 2.3 Prompt frameworks and line editors [P0]
- PSReadLine 2.x, including the inline/list prediction views and the history list.
- oh-my-posh, starship, posh-git.
- bash readline, zsh ZLE.

### 2.4 Full-screen programs (TUIs) that must render correctly [P0]
- vim/neovim, htop/btop, tmux, less, fzf, lazygit, Midnight Commander.
- Mouse reporting (SGR 1006) must work.
- Alternate screen must work.
- 24-bit color must work.
- Box drawing must be gap-free.
- Nerd Font icons must render.
- Emoji and CJK must render (wide characters, Unicode 11 widths).

### 2.5 Keyboard layouts [P0]
- Non-US layouts MUST work. The primary user types on **es-ES**.
- **AltGr** characters MUST reach the shell in every supported shell. On es-ES these include `@ # [ ] { } | \ ~ €`. On Windows, AltGr is reported as Ctrl+Alt.
- Dead keys and IME composition MUST work.

### 2.6 External tools
- **[P0] ShareX scrolling capture.**
  - The terminal pane MUST scroll when ShareX uses its "Windows message" scroll method (`WM_VSCROLL`). See §7.11.
  - The app MUST NOT force itself to run elevated. Windows UIPI blocks synthetic input from non-elevated tools into an elevated window.
- **[P2]** Screen readers (NVDA): use xterm.js `screenReaderMode` as an opt-in.

### 2.7 Displays [P0]
- Per-monitor DPI v2, at 100% through 300%.
- Moving the window between monitors with different DPI MUST re-snap font metrics. There must be no blurry glyphs and no cell gaps.

---

## 3. Architecture

### 3.1 Process and thread model

```
UselessTerminal.exe (Rust, Tauri v2)
├── Main thread: tao event loop, tray, global hotkey, window subclass (WM_VSCROLL, snap layouts)
├── Tokio runtime: IPC command handlers, persistence (debounced atomic writes), SSH helper processes
├── Per pane:
│   ├── pty-reader thread   (blocking ReadFile on the ConPTY output pipe → frames → IPC channel)
│   ├── pty-writer thread   (mpsc queue → WriteFile on the ConPTY input pipe)
│   └── exit-watcher        (wait on the process handle → exit code → orderly ClosePseudoConsole)
├── Log/record writer thread(s): fed by a channel, never by the reader thread directly
└── WebView2 (one browser process group per data folder)
    ├── "main" webview: the entire UI, including ALL xterm.js instances (tabs and panes)
    └── "browser" child webview [P0]: AI browser panel, separate data folder, NO IPC access
```

**[P0] Use one webview for the whole UI.**
- WPF: there is one WebView2 instance per pane, up to 4 per tab, and each pane creates its own `CoreWebView2Environment`. This is the biggest memory and startup cost in the current app, and it also causes the "airspace" layering problems (WPF overlays can't draw on top of WebView2).
- In the rewrite, tabs and panes are DOM elements in a single page, each holding its own `Terminal` instance.
- Hidden tabs keep their xterm instance alive. The xterm buffer keeps parsing output even when the pane is not visible.

### 3.2 Suggested repository layout
```
/crates/ut-pty        ConPTY wrapper: spawn, env block, job object, resize, read/write, exit, kill. Exposes a `Pty` trait.
/crates/ut-vt-scan    Streaming escape/OSC scanner (built on the `vte` crate): OSC 7/9;9/633/1337 cwd, OSC 133, OSC 0/2 title, BEL, OSC 9;4 progress, and the ANSI-stripping sink for logs
/crates/ut-core       Models, settings, themes, sessions, workspaces, snippets, keybindings, persistence + migration, shell detection, SSH arg/config parsing, git HEAD resolver
/src-tauri            Tauri app: commands, channels, capabilities, tray, hotkey, window subclassing, single instance, updater
/ui                   TypeScript + Vite frontend (Solid or Svelte recommended). The xterm instances live OUTSIDE framework reactivity.
```

### 3.3 Recommended crates
These are suggestions; the implementer may substitute equivalents.

- `windows` (windows-rs). Needs at least these features:
  - `Win32_System_Console` (ConPTY)
  - `Win32_System_Threading`
  - `Win32_System_JobObjects`
  - `Win32_System_Pipes`
  - `Win32_Security`
  - `Win32_System_Diagnostics_ToolHelp`
  - `Win32_UI_Shell`
  - `Win32_UI_WindowsAndMessaging`
  - `Win32_Graphics_Dwm`
  - `Win32_System_Registry`
- `vte` — VT parser for the backend scanner.
- `serde`, `serde_json` — persistence.
- `notify` — watching settings files and git HEAD.
- `arboard` or `tauri-plugin-clipboard-manager` — clipboard access, including images and file lists.
- `crossbeam-channel` or `std::sync::mpsc`.
- `parking_lot`.
- `tracing` — logging the app's own diagnostics.
- Tauri plugins:
  - `single-instance`
  - `global-shortcut`
  - `dialog`
  - `notification`
  - `updater` [P1]
  - `window-state`, or a custom equivalent (the custom one is preferred, because tab state must be persisted too)

### 3.4 IPC design (hot path)

#### 3.4.1 Output (backend → UI) [P0]
- Use one Tauri v2 `ipc::Channel` per pane, carrying **raw binary frames**. The JS side receives an `ArrayBuffer` or `Uint8Array` and passes it straight to `term.write()`.
- Do not use base64. Do not use JSON per chunk. Do not build `eval`/`ExecuteScript` strings.
  - WPF: does a base64 encode, string interpolation, `ExecuteScriptAsync`, then `atob` and a per-byte loop in JS. That costs about 4 allocations and copies per chunk.
- Verify that Tauri channels on Windows deliver raw bytes without a JSON round-trip (verify).
- If channel throughput can't meet §19, fall back to a loopback WebSocket bound to `127.0.0.1` on a random port, authenticated by a per-launch random token in the first frame.

#### 3.4.2 Input (UI → backend) [P0]
- `invoke("pty_write", Uint8Array)`. Prefer a raw request body (Tauri v2 `ipc::Request` with a raw body), carrying the pane id in a header or prefix (verify).
- The handler only pushes the bytes onto the pane's writer queue. It MUST NOT block on the pipe.

#### 3.4.3 Control
Use ordinary `invoke` commands with JSON arguments (catalogue in §3.6).

#### 3.4.4 State events (backend → UI)
Use Tauri events (§3.6). Events carry deltas, not full state.

### 3.5 Flow control and batching [P0]

#### Backend reader loop (per pane)
1. Do a blocking `ReadFile` into a 64 KiB buffer.
2. Coalesce. While `PeekNamedPipe` reports bytes are available and the frame is under 64 KiB, read more into the same frame. Never wait just to coalesce: an idle keystroke echo MUST go out immediately.
3. Feed the frame to the `ut-vt-scan` scanner. This is cheap and synchronous, and it only extracts state. Then emit any resulting state events.
4. If logging or recording is on, `try_send` a clone of the frame to the log writer thread. The queue is bounded at 16 MiB. If it is full, drop log data, set a "log truncated" flag, and never block the reader.
5. Send the frame on the pane's channel, then do `unacked += len`.
6. If `unacked > HIGH_WATER` (default 2 MiB), block on a condvar until `unacked < LOW_WATER` (default 512 KiB), the pane is disposed, or 250 ms pass (then re-check).
   - Blocking the reader stops draining the pipe. ConPTY then stops reading the child, so the shell blocks. This is the intended backpressure.

#### Frontend
- Call `term.write(bytes, () => pendingAck += bytes.length)`.
- Flush acks with `invoke("pty_ack", {paneId, bytes})` at most once per animation frame, or immediately once `pendingAck ≥ 256 KiB`.
- Acks are counted only once xterm has **parsed** the data, which is what the write callback signals.
  - WPF: counted only the bytes queued on the host, so xterm's own internal buffer could grow without bound.

#### Other rules
- **Input is never queued behind output.** The writer thread is separate from the reader thread, so Ctrl+C reaches the shell even during a flood.
- **No pre-ready buffer is needed.** The PTY is spawned only after the frontend has a fitted terminal (§5.4).
- **[P2] Reload resilience.** Keep a 1 MiB ring buffer of recent output per pane. If the webview reloads (a crash, or dev hot-reload), the UI can re-attach to running PTYs and replay it.

### 3.6 IPC catalogue

#### Commands (`invoke`)
| Command | Args | Returns | Notes |
|---|---|---|---|
| `pty_spawn` | `paneId, launch: {command \| profileId \| sessionId}, cwd?, env?, cols, rows, startingCommand?, integration: "auto"\|"off", output: Channel` | `{pid, shellKind}` | §4, §6 |
| `pty_write` | raw bytes, paneId | — | non-blocking enqueue |
| `pty_resize` | `paneId, cols, rows` | — | dedup + throttle (§4.6) |
| `pty_ack` | `paneId, bytes` | — | flow control |
| `pty_close` | `paneId, mode: "graceful"\|"force"` | — | §4.8 |
| `pane_info` | `paneId` | `{pid, alive, exitCode?, cwd, cwdHost, ssh?: SshTarget, foreground?: string}` | |
| `shells_detect` | — | `ShellProfile[]` (cached) | §9 |
| `icon_for` | `command` | URL into a custom protocol `uticon://<hash>.png` | §9.3 |
| `sessions_*` | CRUD, move, import/export | | §8 |
| `snippets_*`, `workspaces_*` | CRUD | | |
| `settings_get` / `settings_patch` | partial JSON | effective settings | §14 |
| `keybindings_get` / `keybindings_set` | | | §15 |
| `files_drop` | `paneId, paths[], mode: "copy"\|"paste"` | — | progress via events (§11) |
| `open_external` | `url` | — | scheme allowlist (§18) |
| `open_path` | `path, line?` | — | §5.10 |
| `export_buffer` | `text, suggestedName` | — | native save dialog |
| `log_toggle` / `record_toggle` | `paneId` | new state | §12 |
| `window_state_save` | layout snapshot | — | debounced |
| `notify_attention` | `{paneId, kind}` | — | §12.3 |
| `browser_set_bounds` / `browser_navigate` / `browser_toggle` | | | §17 |

#### Events (`emit` to `main`)
- `pane:exited {paneId, exitCode}`
- `pane:title {paneId, title}`
- `pane:cwd {paneId, path, host}`
- `pane:shell {paneId, kind: "promptStart"|"inputStart"|"commandStart"|"commandEnd", exitCode?, durationMs?}`
- `pane:bell {paneId}`
- `pane:progress {paneId, state, value}` [P2]
- `drop:status {paneId, state, title, detail, severity}`
- `settings:changed {patch}`
- `sessions:changed`, `snippets:changed`, `workspaces:changed`
- `app:quake-toggle`
- `app:second-instance {args}`

**Source of truth for pane state:** title, cwd, exit codes and bell are derived **in the backend**, by `ut-vt-scan`. That way hidden panes, logging and drag-drop never depend on the UI. The frontend parses OSC 133 itself **only** to create xterm markers for command navigation (§6.4).

---

## 4. PTY layer (`ut-pty`)

### 4.1 Spawn sequence (Windows ConPTY) [P0]
1. **Pipes.**
   - Create two anonymous pipes: `in` (we write → ConPTY reads) and `out` (ConPTY writes → we read).
   - The output pipe is 64 KiB; the input pipe uses the default size.
   - Use `SECURITY_ATTRIBUTES` with `bInheritHandle = TRUE` only for the pipe creation call.
2. **`CreatePseudoConsole`.**
   - `CreatePseudoConsole(COORD{cols, rows}, in.read, out.write, flags, &hpc)`.
   - Default `flags = 0`. [P1] Evaluate `PSEUDOCONSOLE_RESIZE_QUIRK` (0x2) with the bundled ConPTY, which reduces reflow garbage on resize (verify).
   - **Do not** use `PSEUDOCONSOLE_INHERIT_CURSOR`.
   - **Do not** use `PSEUDOCONSOLE_WIN32_INPUT_MODE`: xterm.js doesn't produce win32-input-mode sequences.
3. Close our copies of `in.read` and `out.write` immediately. The pseudoconsole owns them now.
4. **Job object** (see §4.8 for its exact role).
5. **Attribute list.**
   - `InitializeProcThreadAttributeList(null, 1, 0, &size)`, then allocate the list, then call it again to initialize.
   - `UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE (0x00020016), hpc, sizeof(HPCON))`. The value passed is the HPCON **value**, not a pointer to it.
6. **`CreateProcessW`.**
   - Arguments: `lpApplicationName = NULL`, `lpCommandLine = <mutable UTF-16 command line>`, `bInheritHandles = FALSE`.
   - Flags: `EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED`.
   - Pass the environment block (§4.3) and `lpCurrentDirectory`.
   - `STARTUPINFOEXW.cb = sizeof(STARTUPINFOEXW)`. Set no std handles.
7. `AssignProcessToJobObject(job, hProcess)`. **If this fails, drop the job and continue. Do not fail the spawn.**
8. **Resume the thread on every path.** `ResumeThread(hThread)` must run whatever happened in steps 4–7.
   - If it returns `(DWORD)-1`, terminate the job or process, close the handles, and return an error.
   - WPF lesson: *"a suspended shell with no output and no error is a silent hang bug."*
9. Start the reader thread, the writer thread and the exit watcher. Return the PID.

**Errors.** Any failure before the process exists MUST clean up everything already created (pipes, the HPCON, the attribute list) and return a typed error. The UI shows the error inside the pane, styled as red text in the terminal, together with the command line that failed.
- WPF: `Start` leaked resources on early failure until the pane was disposed.

### 4.2 Command line handling [P0]
- The command line is passed **verbatim** to `CreateProcessW`. Never split it and re-join it.
- If the user-supplied executable path contains spaces and isn't quoted, quote it **only** when it is the whole command line, or when it is joined with separately stored arguments (§8.1 `GetFullCommand`).
- Paths with spaces are normal here: the user's home and repositories live under `C:\Users\…\OneDrive - BEPSA DEL PARAGUAY SAECA\…`. Every code path that builds or parses a command MUST be tested with such paths.
- Keep command lines within the 32,767 UTF-16 character limit. The PowerShell `-EncodedCommand` payload (§6.5.1) is about 10 KB today. If injection would overflow the limit, skip it and log a warning.

### 4.3 Environment block [P0]
Build a case-insensitive sorted map. Insert in this order; later entries override earlier ones:

1. Defaults: `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=UselessTerminal`, `TERM_PROGRAM_VERSION=<semver>`.
   - Why: with no `TERM`, a remote shell reached over SSH assumes a dumb terminal and **suppresses all color**. This was a real bug: "it works on the main shell, but when I connect through ssh to another server there is no coloring."
2. The parent process environment.
   - [P1] Optionally rebuild it fresh from `HKCU\Environment` plus `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`, expanding `REG_EXPAND_SZ` values. Then new tabs see `PATH` changes without restarting the app. Setting: `terminal.refreshEnvironment = true`.
3. Shell-integration variables (§6.5), for example `PROMPT` for cmd.
4. Per-session overrides (§8.1). These always win.

Serialize the map as `KEY=VALUE\0…\0\0` in UTF-16. Windows requires the block to be sorted case-insensitively.

### 4.4 Reader [P0]
- Described in §3.5.
- The reader never decodes UTF-8. Raw bytes go to xterm, which keeps partial-sequence state across `write()` calls.
- The backend scanner and the logger use **streaming** decoders, so a multi-byte character split across reads is never turned into U+FFFD.

### 4.5 Writer [P0]
- Bytes come from an unbounded mpsc queue and go through `WriteFile` in a loop until fully written.
- A write after the pane is disposed is silently dropped, never an error.
  - WPF lesson: the reader and UI threads race with disposal.
- Mouse reports that xterm emits via `onBinary` are Latin-1 bytes. Send them as raw bytes (one byte per char code), **not** UTF-8.

### 4.6 Resize [P0]
- `pty_resize` is deduplicated against the last size actually applied.
- Throttle it to at most one `ResizePseudoConsole` call per 16 ms, always applying the trailing value.
- Ignore sizes where cols or rows is 0 or below.
- The frontend owns the geometry: it calls `fit()` and then `pty_resize`.

### 4.7 Exit detection [P0]
- A watcher thread waits on the process handle, then calls `GetExitCodeProcess`.
- **Do not** rely on the output pipe reaching EOF to detect exit. ConPTY keeps the output pipe open until `ClosePseudoConsole` is called.
  - WPF: detected exit only through pipe EOF.
- Once the process exits:
  1. Keep the reader draining.
  2. Call `ClosePseudoConsole`. Always do this **while the reader is still draining**: on older Windows builds `ClosePseudoConsole` can block until the output pipe is drained.
  3. Wait for reader EOF.
  4. Emit `pane:exited {exitCode}`.
- **UI** [P0]: print `\r\n[process exited with code N]` dimmed into the pane. Then:
  - [P1] Offer *Enter = restart, Ctrl+W = close*.
  - [P1] Setting `terminal.closeOnExit = "never" | "graceful" (exit code 0) | "always"`, default `"never"` for parity.
  - The tab title gets an "exited" marker.

### 4.8 Closing a pane, and process-tree policy [P0]
Graceful close:
1. Call `ClosePseudoConsole` while the reader is still running. Attached console clients receive `CTRL_CLOSE_EVENT`.
2. Wait up to 100 ms for the shell to exit. WPF lesson: PSReadLine needs a moment to exit cleanly.
3. Terminate the leftover **console-subsystem** processes in the job. Enumerate them with `QueryInformationJobObject(JobObjectBasicProcessIdList)`, read each image's PE subsystem (`IMAGE_SUBSYSTEM_WINDOWS_CUI`), and `TerminateProcess` those.
4. **Spare GUI-subsystem descendants.** For example, `code .` or `notepad` launched from the shell must survive the tab closing.
   - WPF: used `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, which kills GUI apps launched from the shell too. Treat that as a defect (§24).
5. Then close the remaining handles, join the threads (with a 2 s timeout), and free the attribute list.

Job object rules:
- Create it **without** `KILL_ON_JOB_CLOSE`. Its purpose is enumerating the processes, not killing them on close.
- If the app crashes, ConPTY's conhost sees the broken pipes and closes its console clients. That covers crash cleanup for console processes.
- Setting `processes.killConsoleTreeOnClose` (default `true`).

Acceptance tests:
- `ping -t localhost` dies with its tab.
- `node server.js` dies with its tab.
- `notepad` launched from pwsh survives.
- `ssh host` dies.

### 4.9 Elevation [P0]
- The app runs **asInvoker**. Never auto-elevate the whole app: UIPI then blocks ShareX, automation tools, and drag-drop from a non-elevated Explorer.
- ConPTY children inherit the app's token. So "Run as administrator" on a saved session, while the app is **not** elevated, MUST open a **separate elevated console**:
  - `ShellExecuteExW` with `lpVerb = "runas"`, using the session's executable, arguments and working directory.
  - The session's starting command, environment, theme and integration are not applied in that case, and the UI says so.
  - WPF lesson: *"Embedded ConPTY cannot host an elevated child when this process is not elevated."*
- If the app itself is elevated (the user launched it elevated):
  - The title shows `Useless Terminal — Administrator`.
  - "Run as administrator" just opens an embedded tab.
- **[P2]** An elevated broker: a helper process started with UAC that hosts ConPTY and relays I/O over a named pipe protected by an ACL. This makes elevated tabs embeddable without elevating the UI.

---

## 5. Terminal frontend (xterm.js)

### 5.1 Libraries [P0]
- Use the latest stable `@xterm/xterm`, at least 5.5. Pin every version in the lockfile.
- Addons:

| Addon | Priority | Purpose / how to use it |
|---|---|---|
| `fit` | [P0] | sizing |
| `webgl` | [P0] | renderer (§5.3) |
| `unicode11` | [P0] | set `term.unicode.activeVersion = '11'` |
| `search` | [P0] | search bar (§5.11) |
| `web-links` | [P0] | URL detection and clicking |
| `image` | [P0] | sixel and iTerm2 inline images (OSC 1337 File) |
| `clipboard` | [P1] | OSC 52 |
| `serialize` | [P2] | export with colors |
| `ligatures` | [P2] | font ligatures |
| `unicode-graphemes` | [P2] | grapheme clusters, once stable |

WPF shipped: xterm 5.5.0, fit 0.10.0, webgl 0.18.0 (disabled), unicode11 0.8.0, search 0.15.0, web-links 0.11.0, image 0.8.0. No canvas addon was shipped, so the DOM renderer was what actually ran.

### 5.2 Terminal options (defaults; settings override them)
| Option | Value |
|---|---|
| `fontFamily` | `'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace` |
| `fontSize` | 14 |
| `fontWeight` | 400 |
| `fontWeightBold` | `min(900, fontWeight + 250)` |
| `lineHeight` | 1 |
| `letterSpacing` | 0 |
| `cursorStyle` | `'bar'` |
| `cursorBlink` | true |
| `scrollback` | 10000 |
| `allowProposedApi` | true |
| `customGlyphs` | true (box drawing without gaps) |
| `rescaleOverlappingGlyphs` | true |
| `allowTransparency` | true **only** while a background image is active |
| `macOptionIsMeta` | n/a |
| `windowsPty` | `{ backend: 'conpty', buildNumber: <real OS build> }`. This lets xterm apply its ConPTY-specific reflow handling. The backend reports the OS build at startup. |

### 5.3 Renderer policy [P0]
- Use **WebGL by default**. Fall back to the DOM renderer when WebGL fails to load, or on `onContextLoss`: dispose the WebGL addon and keep running.
- **WebGL context budget.** Chromium caps the number of live WebGL contexts per page (about 16; when exceeded, the oldest context is lost) (verify).
  - Keep WebGL attached only to an LRU set of at most **8** panes that were recently visible.
  - Panes that drop out of the set lose their WebGL addon. They get it back when shown again. Re-attaching costs one atlas rebuild, which is acceptable.
- **Ghost-pixel risk.** WPF disabled WebGL because *"its glyph atlas often leaves stale pixels when the shell redraws in place (PSReadLine history/list, prediction) without alternate-screen switches."* The rewrite MUST run the ghosting acceptance tests in §21.3 with WebGL on.
  - If ghosting reproduces, enable the **repair strategy** below, behind `terminal.rendererRepair = "auto" | "on" | "off"`.
- **Repair strategy** (ported from WPF and rate-limited):
  - `forceFullRedraw()`: clear the WebGL texture atlas if WebGL is active, then `term.refresh(0, rows-1)`.
  - After each output burst, wait for 40 ms of quiet in `onWriteParsed`, then run `forceFullRedraw()`. Run it at most **once per 100 ms** during sustained output. WPF did it after every burst, with no cap.
  - On CSI `J` (erase in display), via `registerCsiHandler({final:'J'})` returning `false` so xterm still processes it: cancel the idle timer, then run `scrollToBottom` + `forceFullRedraw` in a microtask, in the next animation frame, and again at +40 ms and +120 ms. WPF lesson: the idle redraw *"races PSReadLine"*.
  - On CSI `K`, on a buffer switch (`onBufferChange`), and on focus: one coalesced redraw within 16 ms.
    - Why the buffer switch: *"switching back to the main buffer does not emit CSI J, so WebGL can keep drawing ghost cells from the TUI over the shell prompt."*
  - On OSC 133;D: idle redraw plus `scrollToBottom`.
- **Solid background.** With no background image, the terminal element has an opaque background and `allowTransparency = false`.
  - WPF lesson: *"allowTransparency + canvas erase (cls) otherwise leaves ghost pixels at viewport bottom."*

### 5.4 Font loading, device pixel ratio and the ready handshake [P0]
1. Before measuring, `await document.fonts.load(\`${size}px ${family}\`)`, then `document.fonts.ready`.
   - If the font loads late, the cell metrics are wrong and the shell caches a bad width.
2. **Snap the font size** to the device pixel grid: `snapped = round(px * dpr) / dpr`. If `px < 8` or is not a number, use 14.
   - Re-snap and re-fit whenever `devicePixelRatio` changes. Detect that with a `matchMedia('(resolution: …dppx)')` listener, or by checking on resize.
   - WPF lesson: *"fractional CSS px × DPR is the usual source of fuzzy canvas text."*
3. **Ready handshake:**
   - Open the terminal, then `fit()` → animation frame → `fit()` → animation frame → `fit()`.
   - Spawn the PTY only once `cols > 0 && rows > 0` and the size has been stable for 2 frames.
   - Pass the real `cols`/`rows` to `pty_spawn`.
   - WPF lessons:
     - *"WebView2 often reports 0×0 cell metrics on first frame; pwsh + oh-my-posh read console size immediately and cache a too-small width if we start too early."*
     - *"if ConPTY starts before WebView has reported its real size … garbled rows in some prompt UIs (PSReadLine list/history, clear)."*
4. After spawning, re-fit at +60, +180 and +400 ms, each inside an animation frame. Send `pty_resize` only if the size changed.
5. **Resize triggers:**
   - A `ResizeObserver` on each pane container, debounced to 50 ms (trailing), followed by `fit()` inside an animation frame.
   - `visibilitychange` back to visible.
   - Window activation.
   - Tab becoming visible (fit once on show; hidden panes never fit).

### 5.5 Keyboard handling [P0]
- **Order of handling:**
  1. App shortcuts (§15) are matched in a capture-phase `keydown` listener on `window`, **before** xterm sees the key.
  2. A matched shortcut calls `preventDefault()` and `stopPropagation()`.
  3. Everything else goes to xterm.
- **Shortcuts are never handled during IME composition** (`event.isComposing` or `keyCode 229`).
- **AltGr is never Ctrl+Alt.** If `event.getModifierState('AltGraph')` is true, the event is text input. Shortcut matching ignores it, and xterm must receive it as a printable character.
  - Acceptance test: type `@` on es-ES (AltGr+2) in pwsh, cmd, bash and over SSH.
- **xterm key interceptions** (`attachCustomKeyEventHandler`, keydown only):
  - **Ctrl+C:**
    - If there is a selection: copy it, then clear the selection (`terminal.clearSelectionOnCopy`, default `true`; WPF didn't clear). Return `false`.
    - Otherwise let it through so `^C` reaches the shell.
  - **Ctrl+Shift+C:** copy if there is a selection; consume it either way.
  - **Ctrl+V, Ctrl+Shift+V, Shift+Insert:** consume, then call `pasteFromClipboard()` (§5.6).
    - Handle paste in exactly one place: either the capture-phase `paste` DOM event, or the keydown, not both.
    - WPF lesson: *"Ctrl+V (and Shift+Insert) fire a native paste event in WebView2 as well as keydown … double paste."*
  - **Ctrl+Insert:** copy.
  - **Ctrl+Shift+F:** search. **Ctrl+Shift+S:** export buffer. Both are configurable (§15).
- **Typed-input color** [P1]: see §6.6.

### 5.6 Clipboard and paste [P0]
- Use **backend clipboard access** (arboard or the plugin), not `navigator.clipboard`, so permissions and formats are reliable.
- `pasteFromClipboard()` checks the clipboard in this order:
  1. **Text** (`CF_UNICODETEXT`). Apply `term.paste(text)` semantics:
     - Normalize line endings to `\r`.
     - Wrap the text in `ESC[200~ … ESC[201~` **only if the program in the pane has enabled DECSET 2004**. xterm.js tracks this state itself.
     - **Sanitize pasted text.** Replace ESC (0x1B) with U+241B `␛`, and strip C1 controls (0x80–0x9F). This stops a pasted `ESC[201~` from breaking out of bracketed paste.
     - WPF forced `ESC[?2004h` on locally, and always bracketed multi-line text. On `cmd.exe`, which has no bracketed paste, that can insert literal garbage. Do not do this.
  2. **[P1] File list** (`CF_HDROP`, e.g. files copied in Explorer). Paste the paths quoted for the pane's shell kind (§11.6), separated by spaces.
  3. **Image only**, with no text:
     - Default (`terminal.pasteImages = "passThroughCtrlV"`): send `\x16` (Ctrl+V) to the PTY. Clipboard-aware programs such as Claude Code or Codex can then read the image from the OS clipboard themselves.
     - Alternative `"inlinePreview"` (WPF parity): render the image locally as an iTerm2 inline image (`ESC]1337;File=inline=1;size=N:<b64>BEL`), written to xterm only and **not sent to the PTY**.
- **[P1] Multi-line paste warning.** `terminal.multiLinePasteWarning = "auto"`: warn only when bracketed paste is off. Show a confirmation dialog with a preview of the first 5 lines.
- **[P2] Large paste warning:** confirm before pasting more than 5 KiB.

### 5.7 Selection and mouse [P0]
- Use xterm's default selection: drag; double-click selects a word, triple-click a line; Alt+drag makes a block selection.
- **Right-click:** a custom context menu. `terminal.rightClick = "menu"` (default) | `"paste"` | `"copyPaste"` (PuTTY/WT style) [P1].
  - Menu items: Copy, Paste, Select All, Clear, Search, Save Output…, then a separator and Split Right / Split Down [P1].
- **[P1]** `copyOnSelect` (default `false`) and `trimTrailingWhitespaceOnCopy` (default `true`).
- Ctrl+click opens links (§5.10). A plain click opens a link only when the shell has no mouse mode active. This matches xterm's default link-provider behavior.

### 5.8 Scrolling APIs [P0]
`window.ut.scrollLines(n)`, `scrollPages(n)`, `scrollToTop()`, `scrollToBottom()`. These are used by the WM_VSCROLL bridge (§7.11) and by keybindings (Shift+PgUp/PgDn, Ctrl+Home/End [P1]).

### 5.9 Font zoom [P0]
- **Ctrl+mouse wheel over a terminal:**
  - Add up `deltaY` until it passes ±40, then step the font size by ±1 within 8–32.
  - Apply locally: snap, `fit()`, `pty_resize`.
- **Ctrl+0** resets to the default size [P1].
- **Ctrl+= / Ctrl+-** step the size [P1].
- **Scope** (`terminal.zoomScope = "global" | "pane"`, default `"global"` for parity):
  - Global zoom updates `settings.terminal.fontSize`, saved with a 500 ms debounce, and applies to every pane that has no session font override.
  - It MUST send only `{fontSize}` to the other panes. WPF re-sent the entire settings payload, including a base64-encoded background image of up to 15 MiB, on **every wheel step**.

### 5.10 Links [P0]
- **URLs:** the web-links addon. On activation, call `invoke("open_external", url)`. The backend enforces the scheme allowlist (§18).
- **OSC 8 hyperlinks:** set `linkHandler` so they open through `open_external`, with the same allowlist. Show the target URL on hover.
- **File paths:** a link provider using this regex (works per buffer line, ported from WPF):
  `/(?:[A-Za-z]:\\[\w\\.\-\s]+(?::\d+)?|\/[\w/.\-]+(?::\d+)?)/g`
  - [P1] Improve it:
    - Join wrapped lines (`isWrapped`).
    - Support `path:line:col`.
    - Support `"quoted paths with spaces"`.
    - Resolve relative paths against the pane's cwd.
    - Recognise UNC paths `\\server\share\…`.
- **`open_path(path, line)`:**
  1. If the path is a directory, run `explorer.exe "<dir>"`.
  2. If the file doesn't exist but its parent does, open the parent in Explorer.
  3. Otherwise, if `code.cmd` or `code.exe` is on PATH, run `code --goto "<file>:<line>"`.
  4. Otherwise, shell-execute the file.
  - [P1] Setting `links.editorCommand`, a template such as `code --goto "{file}:{line}"`.

### 5.11 Search [P0]
- An overlay at the top-right of the pane.
- Controls: input, a result counter, ▲/▼ buttons, an "All tabs" checkbox, ✕.
- **Keys:** Enter = next, Shift+Enter = previous, Esc = close. Closing clears the decorations and refocuses the terminal.
- **Incremental:** search on every input event, debounced 50 ms [P1].
- **Decorations:** highlight matches, with marks in the overview ruler.
- **[P1] Counter:** show "3 of 17" using `onDidChangeResults`. WPF only showed "No results".
- **[P1] Toggles:** regex, case-sensitive, whole word.
- **"All tabs":** run the same query (`findNext`) in every pane of every tab, and open their search bars.

### 5.12 Background image [P0]
- **Setting:** `terminal.backgroundImage.path` plus `opacity` (0–1, default 0.52).
- **How it is served:** through a scoped asset protocol, e.g. `utasset://bg/<hash>`. Never embed it as base64 in IPC, and never re-read it from disk on every settings change.
  - WPF re-read and re-encoded the file (up to 15 MiB) on every apply.
- **Layout:**
  - A layer behind the terminal: `background-size: cover`, centred, no-repeat, `transform: translateZ(0)`.
  - While an image is active, the xterm theme background becomes `rgba(r,g,b,0.62)`, taken from the **theme's** background color. WPF hard-coded black here.
  - `allowTransparency = true`.
- **Limits:**
  - Maximum 15 MiB.
  - Formats: png, jpg, jpeg, gif, webp, bmp.
  - [P2] Stretch mode: cover | contain | tile.

### 5.13 Minimap [P2, parity]
- An overview strip of the scrollback, 48 px wide, on the right edge. It overlays the terminal; [P1] subtract its width from the fit width.
- **Drawing:**
  - Use a DPR-aware canvas.
  - Sample lines with `step = floor(total / min(total, ceil(height / lineHeight)))`.
  - Each bar's width is `len / 120 × (w − 2)` and its brightness is `min(255, 60 + 3·len)`, at alpha 0.5.
  - Stroke the viewport rectangle in `rgba(255,255,255,.3)`, at least 4 px tall.
- **Behavior:**
  - Re-render in an animation frame on `onScroll` and `onRender`.
  - Clicking jumps to `ratio × buffer.length`.
- **Setting:** `terminal.minimap`. It is persisted and applies to new panes. WPF kept it runtime-only, so it was lost on restart and skipped new tabs.

### 5.14 CRT mode [P2, parity]
- **Setting:** `terminal.crt`. It is persisted and applies to new panes.
- **CSS on the pane wrapper:**
  - A flicker animation at 0.15 s, infinite, alternating opacity 0.98 → 1.
  - `::before` scanlines: `repeating-linear-gradient(0deg, rgba(0,0,0,.15) 0 1px, transparent 1px 3px)`, z-index 999, `pointer-events: none`.
  - `::after` vignette: `radial-gradient(ellipse, transparent 60%, rgba(0,0,0,.45) 100%)`, border-radius 12 px.
  - On the terminal: `text-shadow: 0 0 3px rgba(255,255,255,.25)`.
- **The glow MUST be neutral white, never a theme color.**
  - WPF lesson: *"a hardcoded green here washed out every other theme's actual ANSI colors."*
- **[P1]** Honor `prefers-reduced-motion`: no flicker.

---

## 6. Shell integration

### 6.1 Sequences accepted (backend `ut-vt-scan` + frontend) [P0 unless marked]

| Sequence | Meaning |
|---|---|
| `OSC 133;A ST` | prompt start |
| `OSC 133;B ST` | input start, i.e. end of the prompt |
| `OSC 133;C ST` | command output start (pre-exec) |
| `OSC 133;D[;<exit>] ST` | command finished. A **bare `D` means "no exit code available"**. Tolerate `D;0;extra` by cutting at `;`. |
| `OSC 7;file://<host>/<path> ST` | current directory (§6.3) |
| `OSC 9;9;<path> ST` [P1] | ConEmu/Windows Terminal-style cwd (oh-my-posh `pwd: osc99`) |
| `OSC 633;P;Cwd=<path> ST` [P1] | VS Code-style cwd |
| `OSC 633;A/B/C/D` [P1] | treated as aliases of 133 |
| `OSC 1337;CurrentDir=<path> ST` [P1] | iTerm2-style cwd |
| `OSC 0/2;<title> ST` | window/tab title |
| `BEL` (0x07) | bell. Ignore it when it is the terminator of an OSC. |
| `OSC 52;c;<b64> ST` [P1] | clipboard write: allowed by default, size cap 1 MiB. Clipboard **read** (`?`) is denied by default. Setting `terminal.osc52 = "write" | "readwrite" | "off"`. |
| `OSC 9;4;<state>;<pct> ST` [P2] | progress shown on the taskbar icon (ITaskbarList3) and as a tab ring |
| `OSC 9;<text>` / `OSC 777;notify;…` [P2] | desktop notification, only while unfocused |

- Both terminators (BEL and ST = `ESC \`) MUST be accepted everywhere.
- Cap OSC payloads at 8 KiB in the scanner. OSC 52 is the exception, capped at 1 MiB.

### 6.2 Turn state machine [P0]
Each pane has `turn = {phase, promptLine, inputLine, outputLine, exitCode, startedAt}`. The phases are `none | prompt | input | running | done`.

- **A:**
  - Ignored while the alternate screen is active.
  - Otherwise: `phase = prompt`. In the frontend, also register an xterm marker at the cursor line, unless the last marker is already on that line.
  - Keep at most 512 markers; dispose the oldest beyond that.
- **B:** accepted only when `phase == prompt`. Sets `phase = input`.
- **C:** accepted only when `phase == input`. Sets `phase = running` and `startedAt = now`.
- **D:**
  - If `phase == done`: drop it as a duplicate.
  - Otherwise: `phase = done`, `exitCode = parsed | null`, `durationMs = now − startedAt` (only if C was seen).
  - Emit `commandEnd`.
- Transitions only fire from their expected predecessor. So a prompt theme that also emits OSC 133 (oh-my-posh, starship) cannot double-count.

### 6.3 OSC 7 path normalization [P0]
1. Strip `file://`.
2. If what remains doesn't start with `/` or `\`, the text up to the first slash or backslash is the **host**.
3. Percent-decode. If decoding fails, keep the raw text.
4. `/C:/x` or `\C:\x` → `C:\x`.
5. For drive paths, convert every `/` to `\`.

Resulting forms:
- `file:///C:/x` → `C:\x`
- `file:///C:\x` (cmd) → `C:\x`
- `file://host/home/u` → path `/home/u`, host `host`
- Git Bash `/c/Users/x` stays POSIX. The file-drop code maps it to `C:\Users\x` (§11.2).

The host is "local" if it is empty, `localhost`, `127.0.0.1`, `::1`, or equal to the computer name (case-insensitive).

### 6.4 Command navigation [P0]
- **Ctrl+Alt+Up / Ctrl+Alt+Down** jump to the previous or next prompt marker. These move the **viewport only**; the PTY cursor is never touched.
- **Anchor:** `anchor = navAnchor ?? viewportY + 1`.
  - Previous = the last marker with `line < anchor`. Next = the first marker with `line > anchor`.
  - Scroll to `line − 1`, leaving one line of context above, clamped to `[0, length − rows]`.
  - Any user scroll clears the anchor.
- **"Clear" resets the markers.**
- **[P1] Gutter decorations.** A 3 px left-edge mark at each prompt line, colored by the turn's exit code (success = `ui.success`, error = `ui.error`, unknown = `ui.muted`). Hovering shows the exit code and duration.
- **[P1] Scrollbar marks** for failed commands.

### 6.5 Injection, per shell [P0]

**Shell kind detection** uses the lowercased command line, checked in this order:
1. `pwsh` or `powershell` anywhere → **PowerShell**
2. The first token's file name without extension is `ssh` → **Ssh**
3. `wsl.exe`, `\wsl`, or exactly `wsl` → **Wsl**
4. Ends with `cmd.exe` or `cmd.exe"`, ends with `\cmd`, equals `cmd`, or contains `\cmd.exe` → **Cmd**
5. Contains `bash`, `zsh` or `\sh.exe`, or ends with `/sh` → **Posix**
6. Anything else → **Unknown**

Integration is **never injected into Ssh or Unknown shells**: *"we don't control the remote shell."*
Per-session setting: `integration: "auto" | "off"`.

#### 6.5.1 PowerShell (launch-time) [P0]
- **When it applies:** the executable is `pwsh` / `powershell` (with or without `.exe`, with or without a path), **and** the lowercased arguments contain none of `-file`, `-command`, `-encodedcommand`, or ` -c `, and don't start with `-c `.
- **The launch command becomes:**
  `"<exe>" [<args> ]-NoExit -EncodedCommand <base64(UTF-16LE(script))>`
- Why launch-time: *"no race with PSReadLine, no echo-back of injected text, and oh-my-posh/starship can't clobber the prompt because we wrap it AFTER they install theirs."*
- The **starting command** is baked into the script as UTF-8 base64 and run with `Invoke-Expression`, after the user's profile and the integration have loaded.
- The script is reproduced **verbatim** in Appendix A.1. It works on both 5.1 and 7, because it uses `[char]27` and `[char]7` instead of `` `e ``.

#### 6.5.2 cmd.exe [P0]
- **[P1, preferred]** Put `PROMPT` in the **environment block** (§4.3, step 3), unless the session already defines `PROMPT`. cmd reads it at startup, so nothing is echoed, nothing is typed into the shell, and there is no race.
- **[P0 fallback, WPF behavior]** Write this line to stdin 700 ms after spawn:
  `set "PROMPT=…same value…" & cls` followed by `\r\n`
- The `PROMPT` value:
  ```
  $e]133;D$e\$e]133;A$e\$e]7;file:///$P$e\$P$G$s$e]133;B$e\
  ```
  `$e` = ESC, `$e\` = ST (cmd has no escape code for BEL), `$P` = current directory, `$G` = `>`, `$s` = space.
- cmd has no pre-exec hook, so there is no `C` marker, and `D` is always bare.

#### 6.5.3 bash / zsh (Git Bash, MSYS, WSL) [P0]
- **[P1, preferred]** Inject at launch time, the way VS Code does:
  - **bash:** `bash --init-file <tmp>/ut-bash.sh -i`. The file first sources the user's normal startup files (`~/.bashrc`; for login shells, `/etc/profile` then `~/.bash_profile` / `~/.bash_login` / `~/.profile`), then runs the snippet from Appendix A.2.
  - **zsh:** set `ZDOTDIR` to a temp directory whose `.zshrc` sources the user's real `$ZDOTDIR/.zshrc` (or `~/.zshrc`), then runs the snippet.
  - **WSL:** pass the script path translated through `wslpath`, or pipe the script in via an environment variable plus `eval`.
- **[P0 fallback, WPF behavior]** Write the one-line init to stdin 700 ms after spawn, ending in `\n`.
  - Prefix the line with a single space, so `HISTCONTROL=ignorespace` keeps it out of history [P1].
  - Better than a fixed 700 ms [P1]: wait until the first output has arrived, followed by 150 ms of quiet, with a 3 s cap.
- The one-line init:
  ```
  [__UT_OSC7=1; ]if [ -n "$ZSH_VERSION" ]; then <ZSH> elif [ -n "$BASH_VERSION" ]; then <BASH> fi; printf '\033[?2004h'; clear
  ```
  `<BASH>` and `<ZSH>` are the snippets in Appendix A.2 and A.3.
- **WSL** omits `__UT_OSC7=1`. A Linux path doesn't map to a Windows directory.
  - [P2] Instead, keep OSC 7 for WSL and map `/x/y` → `\\wsl.localhost\<distro>\x\y`, for file drops and "open folder".

#### 6.5.4 Starting command (per saved session) [P0]
| Shell | How the starting command is delivered |
|---|---|
| PowerShell | baked into the encoded script |
| cmd | appended to the init input: CRLF/LF normalized, each line ended with `\r` |
| POSIX/WSL | appended to the init line, ending with `\n` |
| Ssh / Unknown | once the process is alive, poll every 200 ms for up to 10 s; then wait 600 ms more and write the command (lines normalized and ended with `\r`) |

[P1] For Ssh and Unknown, instead wait until the first output has arrived plus 500 ms of quiet, with a 10 s cap.

### 6.6 Typed-input color [P1]
The goal: what the user types is shown in `terminal.typedInputColor`, which is a personal setting that theme presets never change (§13.4). Two mechanisms:

1. **PowerShell.** Run `__utApplyInputColor` (Appendix A.1) once per session. It sets every PSReadLine token color (Command, Default, Number, Parameter, Operator, Member, Variable, Keyword, Type, String) to `ESC[38;2;r;g;bm`.
   - Setting `terminal.overridePsReadLineColors` (default `true` for parity).
   - Why it runs late: *"PSReadLine is not imported yet during -EncodedCommand; apply from prompt/OnIdle with 24-bit VT; '#RRGGBB' does not work on older builds."*
2. **Every other shell (mainly cmd).** Write `ESC[38;2;r;g;bm` into **xterm only** (not to the PTY) when entering `phase = input` (OSC 133;B). Write `ESC[39m` locally when the user presses Enter, or when `C` arrives.
   - WPF instead applied it after every OSC 7 and reset it on Enter. That is fragile; use the 133 phases.

---

## 7. Window and UI shell

### 7.1 Main window [P0]
- **Frame:** frameless (`decorations: false`), with a custom title bar 30 px tall.
- **Title bar contents:**
  - Left: a 14×14 accent ring logo, then "Useless Terminal" in muted 11 px text.
  - Centre: the window title (ellipsized).
  - Right: minimize / maximize / close buttons.
- **Size:** default 1200×800, minimum 480×360.
- **[P1] Windows 11 Snap Layouts.** Hovering the custom maximize button must show the snap flyout. Subclass the window and return `HTMAXBUTTON` from `WM_NCHITTEST` for the button's rectangle (verify the approach for Tauri; `tauri-plugin-decorum` is prior art).
- **Resizing:** the window MUST be resizable from every edge and corner at every DPI, even where the terminal reaches the edge.
  - WPF lesson: the WebView2 child window *"would otherwise steal WM_NCHITTEST and block resizing on that side."* WPF reserved a 6 px strip at the right edge.
  - Verify that Tauri's undecorated resize handling works over the webview. If it doesn't, keep a 4–6 px non-terminal margin, or call `startResizeDragging` from edge hot-zones.
- **Title text:**
  - `Useless Terminal` (WPF: "Useless Terminal By Unnamed10110"; keep the credit in the About dialog).
  - With an active pane cwd: `<base>  —  <cwd>`, with two spaces on each side of the em dash.
  - When the app is elevated, `<base>` is `Useless Terminal — Administrator`.
  - The title updates on tab switch [P1] as well as on cwd change. WPF only updated it on cwd change.
- **Backdrop:** `ui.backdrop = "none" | "mica" | "acrylic"`, using Tauri window effects (verify the API).
  - Mica requires Windows 11; on older systems, fall back to `none`.
  - Acrylic on Windows 10 lags while the window is being dragged.
  - When the backdrop is on, chrome surfaces use semi-transparent token colors so the backdrop is actually visible.

### 7.2 Layout [P0]
```
┌ title bar ────────────────────────────────────────────────────────┐
│ sidebar │1px│ tab strip ──────────────────────────────── │5px│ browser │
│ 160–900 │   │ pane area (tabs × split panes)              │   │ 250–1600│
│ (260)   │   │ status bar (22px)                           │   │ (500)   │
└───────────────────────────────────────────────────────────────────┘
```
- **Sidebar and browser panel:** each can be collapsed to 0 and resized with a splitter.
  - Width limits and defaults: sidebar 160–900 px (default 260), browser 250–1600 px (default 500).
  - Widths are saved when a splitter drag ends, but only if they are within those limits.
  - Open/closed state is persisted for both panels. WPF persisted only the sidebar's.

### 7.3 Tabs [P0]

#### Tab strip
- A single line that scrolls horizontally. There is no visible scrollbar.
- The mouse wheel over the strip scrolls it horizontally, unless Ctrl is held.
- The selected tab is scrolled into view with 8 px of padding.

#### Chrome bar
- Bottom border: 2 px accent.
- Left: **Toggle Sessions** (Ctrl+B).
- Right: **Toggle Browser** (Ctrl+Shift+B), **Settings** (Ctrl+,), **New Tab** (Ctrl+T), and **▾ shell menu** (lists the detected shells, §9).

#### Tab header, left to right
1. Pin glyph, when pinned.
2. Shell icon (16×16).
3. Group label (8 px, `#888CF8`).
4. `"{index}  {title}"`. Pinned tabs show no title.
5. 🔒 when read-only (`#FFAA00`).
6. A red dot when logging. [P1] A red ● when recording.
7. A green activity dot (`#00FF44`), when the tab is inactive and has new output or a bell.
8. Close button (✕).

The header uses the scaled UI font. WPF hard-coded 10 px.

#### Selected tab
- A tint of the accent color at about 14% alpha. This tint MUST derive from the **current** accent; WPF left it cyan.
- A 2 px accent bar along the bottom.
- The title always uses `ui.tabForeground`.
  - WPF lesson: a contrast-computed dark foreground on the tinted fill *"made the tab title unreadable."*

#### Tab color
- The palette: None, `#00ff44`, `#ff003c`, `#ffff00`, `#00e5ff`, `#ff00ff`, `#ff8800`, `#ffffff`, `#888888`.
- [P1] A custom color picker as well.
- The color appears as a 7 px dot. It also colors the border: the full color when selected, 50% when not.

#### Context menu
Every label MUST reflect the current state each time the menu opens. WPF went stale after toggles.

- Pin / Unpin
- Rename
- Tab Color ▸
- —
- Split Right / Split Down
- Unsplit All
- Broadcast Input on/off
- Start/Stop Logging
- Start/Stop Recording (.cast)
- Read-Only on/off
- Set Tab Group…
- —
- Duplicate Tab
- Save as Session…
- —
- Close Tab
- Close Other Tabs
- Close Tabs to the Right

#### Creating tabs
| Path | Title | Command | Dir | Color | Title locked |
|---|---|---|---|---|---|
| New tab (Ctrl+T, ＋, tray, palette) | "Terminal" | default profile (§9) | `%USERPROFILE%` | profile color | no |
| Shell menu | a rename dialog, prefilled with a random funny name (a list of about 24 like "Quantum Potato", "Void Chicken") | profile command | `%USERPROFILE%` | profile color | yes |
| Saved session | session name | §8.1 | session dir, or `%USERPROFILE%` | session color | yes |
| Duplicate (Ctrl+Shift+D) | same | same | [P1] the **live cwd** if local; otherwise the original dir | same | same |
| Quick SSH (Ctrl+Shift+O) | `SSH: {input}` | §10.2 | `%USERPROFILE%` | `#6be5ff` | no |
| Workspace / restore | from the stored data | | | | |

[P1] Duplicating a tab copies **everything** about the source: the session's environment, theme and font overrides, the starting command, and the session id. WPF copied only the command and directory.

#### Drag to reorder
- The drag starts after the pointer moves 6 DIP.
- The drop index is the first tab whose midpoint lies to the right of the pointer.
- Use **pointer-event-based** dragging, not HTML5 drag-and-drop. See §11.1 for why.
- [P2] Dragging a tab out of the strip opens it in a new window.

#### Pinning
- Pinning moves the tab to just after the existing pinned tabs.
- Ctrl+W does nothing on a pinned tab that has a single pane. Explicit close actions still close it.

#### Rename
- Renaming sets `titleLocked`, so OSC titles stop updating the tab.
- [P1] Renaming to an empty title unlocks it again.

#### Groups
- [P0] A display-only label. It MUST be possible to clear it; WPF's dialog rejected an empty value.
- [P2] Real groups: collapsible, colored.

#### Closing
- Closing the last tab closes the window.
- The close-confirmation rules are in §7.9.

#### Switching
- Ctrl+Tab and Ctrl+Shift+Tab move to the next/previous tab, with wrap-around.
- Ctrl+1..9 select tab 1–9. The key is consumed only if that tab exists.
- Ctrl+Alt+Numpad1..9 select tabs 1–9, and Ctrl+Alt+Numpad0 selects tab 10.

#### Activity indicator
- Output or a bell on an inactive tab sets the activity dot.
- Activating the tab clears it.

### 7.4 Split panes

#### [P0, parity] Up to 4 panes per tab, in a fixed layout
Splitters are 2 px wide or tall.

| Panes | Layout |
|---|---|
| 1 | full |
| 2 | p0 \| p1 (side by side) |
| 3 | top row p0 \| p1; bottom row p2 spanning the full width |
| 4 | 2×2: p0 p1 / p2 p3 |

#### [P1, target] A binary split tree
- Split Right: `Alt+Shift+=`. Split Down: `Alt+Shift+-`.
- Drag splitters to resize. Pane sizes are kept when panes are added or removed. WPF reset all sizes on every rebuild.
- Layouts are persisted in window state and in workspaces.
- Maximum 16 panes per tab.
- **[P2]** Zoom a pane to fill the tab (`Ctrl+Shift+Z`).

#### New panes
- A new pane inherits the tab's profile, its **live local cwd** [P1], its session environment and theme, and its read-only and broadcast state.
- WPF gave new panes only the original command and directory. They had no environment, theme or read-only flag, and were not logged.

#### Focus
- The focused pane has a 1 px accent border.
- Unfocused panes, when the tab has more than one, get a 50% chrome-colored dim overlay.
- Focus navigation:
  - **[P0]** Ctrl+Shift+Arrow uses the fixed index table:
    - 2 panes: Left/Right toggles between the two.
    - 3 panes: 0→Right→1, 1→Left→0, 0 or 1→Down→2, 2→Up→0.
    - 4 panes: 0 (Right→1, Down→2); 1 (Left→0, Down→3); 2 (Right→3, Up→0); 3 (Left→2, Up→1).
  - **[P1]** Geometric: the nearest pane in that direction.
  - The key MUST pass through to the shell when there is no pane in that direction, including when the tab has a single pane. PSReadLine uses Ctrl+Shift+Arrow to select words. WPF always consumed the key.

#### Closing panes
- Ctrl+W closes the focused pane.
- The pane tree collapses its parent, and focus moves to the sibling.
- **[P0]** Any pane can be closed, including the first. WPF could not close the primary pane while other panes existed.

#### Broadcast input
- **[P0]** Scope: the panes of the current tab. **[P1]** An option to broadcast to all tabs.
- Raw input bytes go **verbatim** to every other target pane, through `pty_write`. That includes `onBinary` mouse data, but only when the target is in the same mouse mode [P2].
- Read-only target panes are skipped.
- WPF stripped CR and LF and then appended `\r` to every chunk, so each keystroke ran as a command. **Do not do this.**
- Show a visible "BROADCAST" badge on the tab and on each pane.

#### Read-only mode
- While read-only, the pane drops all `onData` and `onBinary` input.
- [P1] Read-only state is persisted per tab and inherited by new panes.

### 7.5 Status bar [P0]
- **Size:** 22 px tall. Fields are separated by thin vertical rules. Text uses the small UI font.
- **Fields, left to right:**
  1. **Shell:** the executable's file name without extension, in the success color, after a console icon.
  2. **PID:** `PID {pid}` of the focused pane's shell.
  3. **Cwd:** the focused pane's cwd. Falls back to the tab's initial directory, then `%USERPROFILE%`. Maximum 500 px, ellipsized. For a remote cwd, prefix it with `host:` [P1].
  4. **Git branch,** in the highlight color, after a branch icon. Rules are in §7.6.
  5. **Exit:**
     - `exit: N` in `#FF003C` when N ≠ 0.
     - `exit: 0` in `#22C55E`.
     - Blank before any command has run.
     - A bare `D` keeps whatever was shown before.
     - [P1] Also show the last command's duration, when it is ≥ 1 s.
  - **Right side:**
    - `● running` in `#22c55e`, or `○ exited` in `#6b7280`.
    - [P1] The foreground process name (for example `node`, `vim`), from the pane's process tree. Refreshed at most every 2 s, and only for the focused pane.
- **Updates are event-driven:** on tab or pane focus changes and on `pane:*` events. There are no 5 s polls.
  - WPF used a 5 s timer, plus refreshes at +800 ms and +2.5 s after activating a tab.

### 7.6 Git branch resolution [P0]
Done in Rust, without spawning `git.exe`. Steps:

1. Walk up from the cwd, looking for `.git`.
2. If `.git` is a **directory**, it is the gitdir. If it is a **file** containing `gitdir: <path>` (worktrees and submodules), resolve that path relative to the file. WPF only handled a directory, so worktrees and submodules failed.
3. Read `<gitdir>/HEAD`:
   - `ref: refs/heads/<name>` → `<name>`.
   - Otherwise, if it is 40 hex characters (a detached HEAD) → the first 8 characters.
   - Otherwise → blank.
4. Cache the result per repository root. Re-resolve on cwd change and on 133 `D`/`A` events (which catches `git checkout`). [P1] Also watch the HEAD file with `notify`.
5. Remote (SSH) directories have no branch.

### 7.7 Tray icon [P0]
- Always visible, with tooltip "Useless Terminal".
- **Menu:**
  - Show / Hide (the Quake toggle)
  - New Tab
  - —
  - Settings
  - —
  - Exit
- Double-clicking the icon shows and focuses the window.
- **Never hide the window when it is minimized.**
  - WPF lesson: *"Do not Hide() on minimize — it breaks DWM thumbnails / taskbar previews and can leave a blank surface on restore."*

### 7.8 Quake mode [P0 toggle, P1 drop-down]
- **[P0]** A global hotkey, default **Win+`**, registered by virtual key `VK_OEM_3` with `MOD_WIN | MOD_NOREPEAT`.
  - If the window is visible **and** active, hide it. Otherwise show it, restore it if minimized, activate it, and focus the focused pane.
  - If registration fails, show a non-blocking warning, and let the user pick a different hotkey in Settings.
- The hotkey MUST be configurable. Display it using the label of the current keyboard layout: on es-ES, `VK_OEM_3` is the **Ñ** key. Use `MapVirtualKeyW` with `GetKeyNameTextW`.
- **[P1] Drop-down mode** (`quake.dropdown`):
  - The window slides down from the top of the monitor under the cursor, over 120 ms. Width is 100% of the work area; height is `quake.heightPercent` (default 50%).
  - It stays on top while visible.
  - [P2] It hides when it loses focus (`quake.hideOnBlur`).

### 7.9 Close confirmation [P0]
When closing the window, or a tab with more than one pane, confirm only when something is running:
- **A pane counts as "running"** if any of these is true:
  - its 133 phase is `running`, **or**
  - for shells without integration, the shell process has non-console-host child processes, **or**
  - it is an SSH session whose primary process is `ssh` and is still alive.
- **Message:** "N terminal sessions are still running. Close anyway?" with the default button on Cancel.
- WPF asked whenever any shell was alive at all, which is nearly always.
- **[P1]** A "Don't ask again" checkbox, stored as `processes.closeConfirm = "whenRunning" | "always" | "never"`.

### 7.10 Command palette [P0]
- **Look:** a modal overlay inside the main webview: 480 px wide, at most 460 px tall, accent border, drop shadow.
- **Behavior:**
  - Focus goes to the search box.
  - Esc closes the palette, Enter runs the selected command, Up/Down move the selection (wrapping), double-click runs an item.
  - The palette closes when the window loses focus.
- **[P0] Filtering:** a case-insensitive substring match on the label.
- **[P1] Filtering:**
  - Fuzzy subsequence scoring: bonus for a word start, bonus for consecutive matches, penalty for gaps.
  - Recently used commands rank first.
  - Each item's shortcut text is shown on the right, read from the **live** keybindings. WPF hard-coded these strings.
- **Commands** (id → label):
  - `newTab` → New Tab
  - `closeTab` → Close Tab / Pane
  - `togglePanel` → Toggle Sessions Panel
  - `toggleBrowser` → Toggle Browser Panel
  - `settings` → Open Settings
  - `duplicateTab` → Duplicate Tab
  - `addSession` → New Saved Session
  - `splitPane` → Add Pane (Split)
  - `unsplitAll` → Unsplit All Panes
  - `renameTab` → Rename Tab
  - `pinTab` → Pin / Unpin Tab
  - `broadcastToggle` → Toggle Broadcast Input
  - `nextTab` → Next Tab
  - `prevTab` → Previous Tab
  - `closeOthers` → Close Other Tabs
  - `closeRight` → Close Tabs to the Right
  - `quake` → Toggle Window (Quake Mode)
  - `quickConnect` → Quick SSH Connect
  - `toggleLog` → Toggle Session Logging
  - `toggleReadOnly` → Toggle Read-Only Mode
  - `toggleRecording` → Toggle Recording (asciicast)
  - `toggleCrt` → Toggle Retro CRT Mode
  - `findAllTabs` → Find in All Tabs
  - `toggleMinimap` → Toggle Minimap Scrollbar
  - `saveWorkspace` → Save Current Tabs as Workspace
  - `ws:<id>` → Open Workspace: {name}, one entry per workspace
- **[P1] Additional entry types:**
  - `Open Session: {name}` for every saved session.
  - `Run Snippet: {name}`.
  - `Go to Tab: {title}`.
  - `Theme: {preset}`, which live-previews the theme on hover or selection and reverts if you cancel.
  - `Manage Workspaces…` (rename / delete).

### 7.11 External scroll bridge for ShareX [P0]
- Subclass the top-level window **and** the WebView2 host child window(s) with `SetWindowSubclass`. Handle `WM_VSCROLL` (0x0115) using the low word of `wParam`:

  | Low word | Action |
  |---|---|
  | `SB_LINEUP` (0) | scroll the focused pane by −1 line |
  | `SB_LINEDOWN` (1) | +1 line |
  | `SB_PAGEUP` (2) | −1 page |
  | `SB_PAGEDOWN` (3) | +1 page |
  | `SB_TOP` (6) [P1] | scroll to top |
  | `SB_BOTTOM` (7) [P1] | scroll to bottom |

- Forward the action to the focused pane through an event.
  - WPF lesson: *"notably ShareX's 'scrolling capture' feature in its Windows-message scroll mode."*
- **[P1]** Expose a native vertical scrollbar through UI Automation, the scroll pattern, so that capture tools can find a scrollable element (verify feasibility with WebView2).
- **Acceptance test:** ShareX → Scrolling capture → "Windows message" method on a pane holding 500 lines of scrollback captures the whole buffer, with ShareX not elevated.

### 7.12 Dialogs
All dialogs are rendered inside the main webview, never as native windows. That avoids the airspace issues (§17).

- **Rename:**
  - 360 px wide.
  - All text selected on open.
  - Enter = OK, Esc = cancel.
  - The result is trimmed.
  - Whether an empty value is allowed depends on context: allowed for tab groups and titles, not for folder names.
- **Session edit:** §8.3.
- **File conflict:** §11.4.
- **Confirmations:** described in the places that use them.

---

## 8. Sessions sidebar and data

### 8.1 Data model [P0]

#### Session
| Field | Type | Default | Notes |
|---|---|---|---|
| `id` | string | 32-hex GUID (no dashes) | stable; tabs link back to it |
| `name` | string | "New Session" | becomes the tab title (locked) |
| `description` | string | "" | tooltip |
| `shellPath` | string | "" | executable; may be a full command line for WT imports (§8.6) |
| `arguments` | string | "" | raw argument string |
| `workingDirectory` | string | "" | blank means `%USERPROFILE%`. [P1] Support `%VAR%` and `~` expansion. |
| `startingCommand` | string | "" | §6.5.4 |
| `colorTag` | string | `#00ff44` | card tint, dot, tab color |
| `iconOverride` | string | "" | [P1] a path to a .ico/.png/.exe; blank means the shell's icon |
| `folderId` | string? | null | null means root |
| `sortOrder` | int | 0 | order within its folder |
| `themeBackground` | string | "" | per-session terminal background override |
| `themePreset` | string | "" | [P1] a whole preset applied to this session's panes |
| `fontSize` | int | 0 | 0 means the global size; otherwise 8–32 |
| `environment` | string | "" | lines of `KEY=VALUE` |
| `integration` | "auto"\|"off" | "auto" | [P1] |
| `runAsAdmin` | bool | false | [P2] the default launch mode |

**`GetFullCommand()`:**
- If `arguments` is blank: return `shellPath` trimmed. Quote it if it contains a space and isn't already quoted, **unless** it already contains arguments (the WT-import case, detected by checking whether it resolves to an existing file).
- Otherwise: `"<shellPath>" <arguments>`.
- WPF quoted entire imported command lines such as `"wsl.exe -d Ubuntu"`, which breaks them.

**Environment parsing:**
- Split on `\n` and trim `\r`.
- Skip empty lines and lines whose first `=` is at index ≤ 0.
- Trim the key and the value.
- Keys are case-insensitive; the last duplicate wins.
- [P1] Expand `%VAR%` references against the base environment.

#### Folder
`{ id, name: "New Folder", sortOrder }`
- **[P0]** Folders exist only at the root: one level of nesting.
- **[P2]** Nested folders, using `parentId`.

#### Snippet
`{ id, name, command, appendEnter: true [P1], sortOrder }`

### 8.2 Sidebar UI [P0]

#### Header
- The title "Sessions", in the accent color.
- A **"+ New" pill** (accent tint, add icon) with tooltip "New Session (Ctrl+Shift+N)".
- An **overflow ⋯ menu:** Export sessions…, Import sessions…, New folder.

#### Search box
- The border turns accent-colored on focus.
- [P1] Placeholder text "Search sessions".

#### Tree
- Root folders come first, ordered by `sortOrder`, then root sessions.
- **Folder card:**
  - Chevron toggle, folder glyph, name, and a count pill.
  - **Hover-reveal actions:** + new session in folder, rename, delete.
  - The selected folder has a tint and an accent border.
- **Session card:**
  - A 7 px color dot, the 20 px shell icon, the name, and the display command (small and muted).
  - On the right: a **live dot** (shown when any open tab came from this session) plus edit and delete buttons.
  - Tint = `colorTag` at 15% (28% when selected). Border = `colorTag` at 40%.
- **Indent guides:** a 1 px left border on the children container, offset 14 px.
- **Expansion state** is persisted per folder [P1]. WPF re-expanded every folder on each refresh.
- **[P0] Live-session dot** is event-driven, updated on tab open and close. WPF polled every 2 s.
  - Every launch path that came from a session sets `sessionId` on the tab: the panel, workspaces, duplicates, and restores.

#### Selection
- A click selects one item.
- **Ctrl+click** toggles multi-select.
- [P1] **Shift+click** selects a range.
- Multi-select is used for drag-moving several sessions at once. [P1] Also for bulk delete and bulk color change.

#### Keyboard [P1]
- Up/Down: move.
- Enter: open.
- Ctrl+Enter: run as administrator.
- F2: rename.
- Del: delete (with confirmation).
- Left/Right: collapse / expand a folder.

#### Launching
- Double-click: open embedded.
- **Ctrl+double-click:** run as administrator (§4.9).
- Double-clicks on buttons are ignored.

#### Search
- Trim the query. A blank query shows the tree.
- Otherwise show a **flat** list of sessions whose name or command contains the query (case-insensitive), ordered by `sortOrder`.
- [P1] Also match the description and arguments, and highlight the matched text.

#### Drag and drop
Pointer-event based (see §11.1). Threshold: 6 DIP.
- **What is dragged:** if the dragged session is part of the multi-selection, the whole selection is dragged, ordered by folder and then `sortOrder`.
- **Drop rules:**

  | Dragged | Dropped on | Result |
  |---|---|---|
  | Sessions | folder | appended to that folder |
  | Sessions | a session | inserted before it (top half) or after it (bottom half); moves into the target's folder. No-op if dropped on one of the dragged items. |
  | Sessions | empty space | moved to root and appended |
  | Folder | another folder | before (top half) or after (bottom half) |
  | Folder | anything else | moved to the end |

- Use the **header row height** for the top-half / bottom-half test. In WPF, a folder's height included its children, so almost every drop on a folder counted as "top half".
- After every move, renumber `sortOrder` to 0..n−1 in every affected container, then save.

#### Context menus
- **Tree background:**
  - New session
  - New folder
  - —
  - Import from Windows Terminal
  - Import from SSH Config
  - —
  - Export sessions…
  - Import sessions…
- **Folder:**
  - New session in folder
  - Rename
  - Move up
  - Move down
  - —
  - Expand
  - Collapse
  - —
  - Delete folder ("Sessions in this folder will be moved to the root list.")
- **Session:**
  - Open
  - Run as administrator
  - Edit
  - Duplicate
  - [P1] Copy command line
  - —
  - Delete

#### Seeding
On first run only (when no sessions file exists), create one session per detected shell.
- WPF also re-seeded whenever the list was empty. That means a user who deliberately deleted every session got them all back, and a corrupt file got silently overwritten. **Do neither.**

#### Snippets section, at the bottom of the sidebar
- Collapsible, with a maximum height of 200 px.
- Each item shows the name above the muted command, plus a delete button.
- **Add:** one dialog with Name, Command (multi-line), and "Press Enter after sending".
- **[P1] Edit** the same dialog via double-click on an item's edit icon, or F2.
- **Run:** double-click (or Enter) sends the command to the **focused pane**.
  - Multi-line commands use bracketed paste when the shell supports it.
  - `\r` is appended when `appendEnter` is set.
  - When broadcast is on, the snippet goes to every broadcast target.

### 8.3 Session edit dialog [P0]
Fields, in order:
1. **Name** (required).
2. **Description** (multi-line).
3. **Preset shell:** "— Custom —" followed by the detected shells. Choosing one fills in the path, arguments and color.
   - The preset re-syncs when Path or Arguments change [P1]. WPF only re-synced after Browse.
4. **Shell path** (required), with a Browse… button (filter `*.exe`).
5. **Arguments.**
6. **Working directory,** with a folder picker.
7. **Starting command,** with the hint "Runs after shell starts (e.g. ssh user@host, cd /project)".
8. **Theme override:**
   - Background color (a hex field with a picker).
   - Font size (0 means global, otherwise 8–32).
   - [P1] Theme preset.
9. **Color tag:** six swatches (`#00ff44`, `#ff003c`, `#ffff00`, `#00e5ff`, `#ff00ff`, `#ff8800`) **plus a custom color** [P0].
   - An existing color that isn't in the swatch list MUST be kept as-is. WPF silently reset it to green.
10. **Environment variables** (KEY=VALUE, one per line), with [P1] inline validation.
11. [P1] **Shell integration:** auto / off.
12. Buttons: **Cancel**, **Save**.

Validation:
- Name and path are required.
- The font size is reset to 0 when it is outside 8–32.
- [P1] Warn, but don't block, when the path doesn't resolve or the working directory doesn't exist.

### 8.4 Persistence of sessions [P0]
- **File:** `%APPDATA%\UselessTerminal\sessions.json`:
  ```json
  { "schemaVersion": 3, "folders": [ … ], "sessions": [ … ], "snippets": [ … ] }
  ```
  Snippets can live in this file or stay in their own file.
- **Writes are atomic** (§16.2) and debounced by 300 ms.
- **On a parse error:**
  - Do **not** overwrite the file.
  - Rename it to `sessions.corrupt-<timestamp>.json`.
  - Show a banner: "Sessions file was unreadable and was backed up; starting empty."
- **[P1]** Watch the file and reload it when another process changes it.

### 8.5 Import and export [P0]
- **Export:** a native Save dialog, default file name `useless-terminal-sessions.json`. Writes the v3 root object.
- **Import:**
  - Accepts v3, **legacy v2** (`{Version, Folders, Sessions}`, PascalCase keys), and **legacy v1** (a bare `SavedSession[]` array).
  - **[P1]** Ask **Merge** or **Replace**. WPF only offered replace.
  - Merge regenerates ids that collide and de-duplicates by name + command.

### 8.6 Windows Terminal import [P0]
- **Look for `settings.json` in this order:**
  1. `%LOCALAPPDATA%\Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\settings.json`
  2. The same path under `…WindowsTerminalPreview_8wekyb3d8bbwe…`
  3. `%LOCALAPPDATA%\Microsoft\Windows Terminal\settings.json` (unpackaged install)
- **Parse it as JSONC:** strip comments (string-aware), allow trailing commas.
- **Profiles** come from `profiles.list[]`, or from `profiles` when it is itself an array.
- **Skip** profiles with `hidden: true`. Read `hidden` tolerantly, so a non-boolean value doesn't abort the whole import.
- **Profiles with `commandline`:** the session's `shellPath` holds the full command line and `arguments` is empty (§8.1).
- **[P1] Dynamic profiles** (no `commandline`, but a `source`):
  - Map `Windows.Terminal.Wsl` to the WSL distro named by `name`.
  - Map `Windows.Terminal.PowershellCore` to the detected pwsh.
- **Field mapping:**
  - Name: `[WT] {name}`.
  - Directory: `startingDirectory`, with `%USERPROFILE%` and other `%VAR%` references expanded.
  - Color tag: `#00e5ff`.
  - [P1] `tabColor` → `colorTag`, and `icon` → `iconOverride`.
- **De-duplicate** by exact name.
- **Report** the number actually added, not the number found. WPF reported the number found.

### 8.7 SSH config import [P0, improved]
- **Read** `%USERPROFILE%\.ssh\config`.
  - [P1] Follow `Include` directives (globs, relative to `~/.ssh`).
- **Parsing:**
  - Keys are case-insensitive.
  - Key and value are separated by whitespace **or** `=`.
  - Values may be quoted.
  - Blank lines and lines starting with `#` are skipped.
- **Blocks:**
  - Each `Host` line may list several patterns. Every pattern without `*`, `?` or `!` becomes **one** session.
  - `Match` blocks are skipped.
- **[P1] Each session is created as `ssh <alias>`,** rather than expanding HostName, User and Port into flags. That way ssh applies the user's whole configuration itself: ProxyJump, IdentityFile, options, and so on.
  - The HostName, User and Port are shown in the description only.
  - WPF expanded only HostName, User, Port and IdentityFile, and lost everything else.
- **Session fields:**
  - Name: `[SSH] {alias}`.
  - Shell path: the ssh executable, found by §10.1.
  - Color: `#6be5ff`.
- **De-duplicate** by name.
- **Messages:**
  - "Imported N SSH host(s) from ~/.ssh/config."
  - "No SSH hosts found…"

---

## 9. Shell detection and icons

### 9.1 Detection [P0]
Detection runs **off the UI thread** and **caches its result**. Re-run it only when the user opens the shell menu and more than 60 s have passed, or when they click "Refresh shells".
- WPF ran it synchronously, sometimes twice per new tab, and it included a `wsl --list` call with a 3 s timeout.

Profiles, in this order:
1. **PowerShell** (`pwsh.exe`), color `#00e5ff`. **Default** profile if found. Search, in order:
   - `PATH`
   - `%ProgramFiles%\PowerShell\7\pwsh.exe`
   - `%ProgramFiles%\PowerShell\7-preview\pwsh.exe`
   - `%LOCALAPPDATA%\Microsoft\WindowsApps\pwsh.exe` (Store app execution alias)
   - `%USERPROFILE%\.dotnet\tools\pwsh.exe`
2. **Windows PowerShell:** `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`, color `#00e5ff`. Default if there is no pwsh.
3. **Command Prompt:** `%SystemRoot%\System32\cmd.exe`, color `#ffff00`.
4. **WSL**, if `System32\wsl.exe` exists:
   - A generic "WSL" profile, color `#ff8800`.
   - One `WSL: {distro}` profile per distro, with arguments `-d "{distro}"` (quoted).
   - **[P1] Read the distros from the registry:** the `DistributionName` value of each subkey of `HKCU\Software\Microsoft\Windows\CurrentVersion\Lxss\{GUID}`. The default distro is the `DefaultDistribution` value.
     - This is faster than running `wsl.exe --list --quiet`, and avoids that command's UTF-16 output. WPF worked around the encoding by stripping NUL characters.
   - Hide `docker-desktop` and `docker-desktop-data`.
5. **Git Bash**, with arguments `--login -i`, color `#ff003c`. Try, in order:
   - `HKLM\SOFTWARE\GitForWindows\InstallPath` + `\bin\bash.exe` [P1]
   - `%ProgramFiles%\Git\bin\bash.exe`
   - `%ProgramFiles(x86)%\Git\bin\bash.exe`
6. **[P2]** MSYS2 (`C:\msys64\usr\bin\bash.exe`, with `MSYSTEM=UCRT64` and `CHERE_INVOKING=1`), Cygwin, Nushell, Visual Studio developer shells.
7. **Fallback** when nothing was found: Command Prompt, as the default.

**Default profile:**
- Set by `shells.defaultProfile`: either `"auto"` (the first profile marked default above) or a profile or session id. [P1]
- The command for a profile is its executable, quoted if it contains a space, followed by ` {arguments}`.

### 9.2 Executable resolution [P0]
Used for icons and shell-kind detection.
1. Trim surrounding quotes.
2. If the path is quoted, the executable is the text between the quotes.
3. If it is unquoted and contains spaces, add one word at a time until the prefix resolves to an existing file. WPF lesson: naive parsing *"would stop at the first space, e.g. C:\Program."*
4. Try, in order: an exact path; the current directory; `System32`; `System32\WindowsPowerShell\v1.0` (for powershell only); each `PATH` entry with each `PATHEXT` extension.

### 9.3 Icons [P1]
- **Extraction:** `SHGetFileInfoW` (or `ExtractIconExW`) on the resolved executable, at 16 px and 32 px. Convert the HICON to PNG.
- **Caching:** cache in memory and on disk at `%LOCALAPPDATA%\UselessTerminal\icon-cache\<sha1(path+mtime)>.png`.
- **Serving:** through `uticon://`.
- **Fallback:** the app icon.
- Every WSL distro shows the `wsl.exe` icon. [P2] Use distro-specific icons.

---

## 10. SSH

### 10.1 Locating ssh [P0]
Try, in order:
1. `%SystemRoot%\System32\OpenSSH\ssh.exe`
2. `%ProgramFiles%\Git\usr\bin\ssh.exe`
3. `ssh` on `PATH`

`scp` is looked for next to the ssh executable first, then on `PATH`.

### 10.2 Quick connect (Ctrl+Shift+O) [P0, improved]
- **Dialog:** "Quick SSH Connect (user@host or user@host:port)". [P1] Show a history dropdown of the last 20 targets.
- **Grammar:**
  - `[ssh://][user@]host[:port]`
  - IPv6: `[user@][addr]:port`, or a bare IPv6 address with no port.
  - Input that starts with `-` or contains a space is passed through as raw `ssh` arguments.
  - WPF's "last colon" rule broke IPv6 addresses.
- **Command:** `"<ssh>" [-p <port>] [user@]host`
- **Tab:** title `SSH: {input}`, color `#6be5ff`.
- **[P1]** A "Save as session" button in the dialog.

### 10.3 Colors over SSH [P0]
Covered by the `TERM` / `COLORTERM` defaults in §4.3.

### 10.4 SSH argument parsing (`SshTarget`) [P0]
Parsing serves file uploads and remote-cwd detection.

**Tokenizer:**
- A `"` toggles quoted mode.
- Whitespace separates tokens outside quotes.
- There are no backslash escapes, matching Windows behavior.

**Rules:**
- `argv[0]` must have the file name `ssh` (with or without extension).
- **Options read:**
  - `-p <port>` (an integer)
  - `-l <user>`
  - `-i <identity>`, with `~` expanded to `%USERPROFILE%`
  - `-F <config>`, with the same expansion
  - `-J <jump>`
  - `-o K=V`
- These options also accept a value attached to the flag, for example `-p2222`.
- **`-o` handling:**
  - `ControlPath` is captured.
  - `ControlMaster`, `ControlPersist` and `BatchMode` are dropped.
  - Every other `-o` option is kept and passed on to helper commands.
- **Flags that take a value** (skip the next token): `-B -b -c -D -E -e -F -I -i -J -L -l -m -O -o -p -Q -R -S -W -w`.
- **Destination:**
  - After `--`, the next token is the destination.
  - Otherwise, the first positional token is the destination. It is split on its **last** `@`.
  - A user given with `-l` wins.
  - [P1] Accept `ssh://user@host:port` URIs.
- The remaining positional tokens are the remote command. Ignore them.
- **[P1] Resolve aliases** through `ssh -G <dest>`, which prints the effective `hostname`, `user`, `port`, `identityfile` and `proxyjump`.
  - Cache the result per destination for 60 s.
  - This replaces guessing from argv alone.

**Locating SSH for a pane:**
1. Parse the pane's own command. If it parses as ssh, that is the target, marked `isPrimaryShell`.
2. Otherwise, walk the shell's **process tree**:
   - `CreateToolhelp32Snapshot` gives parent/child relationships.
   - The command line of a child comes from `NtQueryInformationProcess(ProcessCommandLineInformation = 60)`, read as a UNICODE_STRING.
   - The first descendant whose command line parses as ssh is the target.
3. Otherwise, if the OSC 7 host is not local, use a minimal target `{host}`.

**Performance:** cache the result per pane. Invalidate it on `pane:title`, on `pane:shell commandEnd`, or after 2 s.
- WPF took a full process snapshot on **every drag-over event**.

### 10.5 Connection reuse (ControlMaster) [P1, capability-gated]
- **WPF behavior:** for every ssh command without a ControlPath, it injected:
  ```
  -o ControlMaster=auto -o ControlPersist=yes -o ControlPath=%LOCALAPPDATA%/UselessTerminal/ssh-mux/%C
  ```
  The goal was that a later `scp` could reuse the session's login, password logins included.
- **Required:**
  - Only inject when the ssh build **supports multiplexing**. Win32-OpenSSH historically does **not** (verify on the current version). MSYS/Git ssh generally does.
  - Probe once per ssh executable and cache the answer. For example, run `ssh -o ControlMaster=auto -o ControlPath=<tmp> -O check localhost` and classify its stderr.
  - Setting `ssh.connectionReuse = "auto" | "always" | "never"`.
- **ControlPath directory:** `%LOCALAPPDATA%\UselessTerminal\ssh-mux`, with an ACL that gives access to the current user only. Use forward slashes in the path, and quote it if it contains spaces.

### 10.6 Remote current directory [P0]
Sources, in priority order:
1. OSC 7 (or another cwd OSC) with a non-local host.
2. **Title fallback,** used only for SSH panes and only until OSC 7 has been seen: the text after the first `:` in the title, trimmed, accepted if it starts with `~`, `/` or `.`. This matches titles like `user@host: ~/dir`.
3. `~`.

**[P2] Remote integration helper.** A palette command "Copy remote shell-integration snippet" that puts the bash/zsh snippet (Appendix A.2/A.3) on the clipboard, with `__UT_OSC7=1` and a hostname-aware OSC 7: `printf '\033]7;file://%s%s\007' "$HOSTNAME" "$PWD"`. The user pastes it into the remote `~/.bashrc`.

---

## 11. File drag-and-drop

### 11.1 Drag-and-drop mechanics in Tauri [P0]
- **External file drops:** use Tauri's native drag-drop events. They provide paths and a physical position.
  - Convert the position to logical coordinates (`÷ scaleFactor`), then find the pane under it with `document.elementFromPoint`.
- **Internal drag-and-drop** (tabs, sidebar items) MUST use pointer events, not HTML5 drag-and-drop.
  - On Windows, enabling Tauri's file-drop handler disables HTML5 drag-and-drop inside the webview (verify for the Tauri version in use).
  - If that turns out not to be true, either approach is fine.
- **Hover overlay:**
  - While a file drag is over a pane, show a card: "Drop to copy", with the destination folder (or "Release to copy into this folder").
  - The overlay follows the pane under the cursor.

### 11.2 What a drop does [P0]
- **Default:** **copy the dropped files into the pane's current directory.**
  - **Local pane:** copy on disk.
  - **SSH pane:** upload to the remote cwd.
- **[P1] With Shift held** (or by setting `drop.defaultAction = "paste"`): insert the dropped paths instead, quoted for the pane's shell kind (§11.6). This is the Windows Terminal behavior.

**Local destination:**
1. The pane's cwd, if it exists on disk.
2. Otherwise, convert a Unix-style path first:
   - `/mnt/x/…` → `X:\…`
   - `/x/…` → `X:\…`
   - `x:/…` → `x:\…`
3. Otherwise, `%USERPROFILE%`.

**Remote destination:** the remote cwd (§10.6), if usable; otherwise `~`.
A remote cwd is "usable" when it is non-blank **and** one of these holds:
- the OSC 7 host is not local, or
- it starts with `~`, or
- it starts with `/` and does not exist as a local directory.

### 11.3 Overlay states [P0]
| State | Title | Detail | Color | Auto-hide |
|---|---|---|---|---|
| hover | Drop to copy | destination | accent | — |
| checking | Checking… | `name → dest` / `N items → dest` | accent | — |
| copying | Copying… / Copying N items… | `Copying i of N… {name}` | accent | — |
| success | Copied / Copied N items | up to 4 names, then "… and N more" | success | 3.2 s |
| partial | Copied X, Y failed | first 4 errors | warning | 5 s |
| failed | Copy failed | first 4 errors, or "Nothing was copied." | error | 5 s |
| no cwd | Cannot copy | "The current shell directory is not available yet." | error | 4 s |

[P1] For large transfers, show a byte-level progress bar and a Cancel button.

### 11.4 Name conflicts [P0]
**Detecting conflicts:**
- **Local:** the destination path already exists and is not the same path as the source.
- **Remote:** for each item, run
  `ssh <conn args> <target> "test -e <q> && echo UT_EXISTS || echo UT_MISSING"`
  with a 20 s timeout. If the result can't be determined, treat it as "no conflict".
  - [P1] Check every item in a single round-trip, with one script.

**Dialog: "File already exists"**
- **Headline:** "N items already exist in the destination." or "A file with this name already exists."
- **Destination** label.
- **Names:** up to 6. When there are more, show 5 plus "… and N more".
- **Hint:** "Rename keeps both copies as "{stem} (1){ext}". Replace overwrites the existing item(s)."
- **Buttons:** Cancel, **Rename** (the default), Replace (warning style).
- **[P1]** "Skip" and "Apply to all".

**Rename:**
- Local: try `"{stem} ({n}){ext}"` for n = 1..999, then a GUID suffix.
- Remote: the same naming scheme, probing each candidate with `test -e`.

**Replace:**
- Local: delete first. Files get their attributes reset before deletion. Folders are deleted recursively.
- Remote: `rm -rf <q>` with a 2 min timeout.
  - **Refuse** when the path is `""`, `.`, `/`, `~` or `~/`.

### 11.5 Transfer [P0]
**Local copies:**
- Files: copy with overwrite.
- Folders: recursive copy.
- **Refuse** to copy a folder into itself.
- [P1] Use `CopyFileExW`, which gives progress reporting and preserves attributes.

**Remote uploads:**
- Command: `scp [-r] -o BatchMode=yes [mux opts] [-P port] [-i id] [-F cfg] [-J jump] [extra -o] <local> <target>:<remotePath>`
  - Uploads have a 2 h timeout. A timeout kills the whole process tree.
- **Quoting:** helper `ssh` commands quote remote paths with POSIX single quotes (`'` → `'"'"'`).
- **Leading `~` must be expanded remotely:** send `~/rest` as `"$HOME"/'rest'`.
  - WPF single-quoted the whole path, so `~` stayed literal: `test -e` and `rm -rf` operated on a directory actually named `~`.
- **[P1] Prefer `sftp -b -`** (batch mode reading commands from stdin) over scp when available. It handles paths more consistently.

**Error formatting:**
- If the output contains one of these, report its first line:
  - "Permission denied"
  - "Host key verification failed"
  - "Connection refused"
  - "Connection timed out"
  - "Could not resolve"
- Empty output: "scp exited N. For password logins, open SSH as its own tab so the drop can reuse that connection."
- A first line mentioning "password": append " Open SSH as its own tab (Quick Connect or a saved session) so drops reuse the login."

**[P2] Fallback when there are no keys and no multiplexing:** an in-band upload through the pane's own PTY.
- Use either a trzsz-compatible protocol, or a POSIX `base64 -d > file <<'EOF'` heredoc, limited to files ≤ 10 MiB, and only when the pane is at a prompt (133 phase = input).
- This works across any authentication method and through nested hops.

All of this work runs off the UI thread.

### 11.6 Quoting a path for each shell [P1]
| Shell kind | Quoting |
|---|---|
| PowerShell | `'…'`, with `'` doubled; or `& '…'` when it is the first token |
| cmd | `"…"` |
| POSIX / Git Bash | `'…'` with POSIX escaping. Git Bash also converts `C:\x` → `/c/x`. |
| WSL | `/mnt/c/x`, single-quoted |
| SSH | the path as-is, single-quoted (it is a local path: the user's choice) |

---

## 12. Logging, recording, notifications

### 12.1 Session logging [P0]
- **Toggle:** per tab. [P1] Logs every pane of the tab, prefixing each line with the pane's index when there is more than one pane. WPF logged only the primary pane.
- **File:** `%APPDATA%\UselessTerminal\logs\<sanitized title or "session">-yyyyMMdd-HHmmss.log`
  - UTF-8, no BOM.
  - First line: `--- Session log started: <ISO-8601> ---`. Last line: `--- Session log ended: <ISO-8601> ---`.
- **ANSI stripping:** feed the bytes through a real VT parser (`vte`) and keep only printable text, LF, and TAB.
  - Drop CSI, OSC, DCS, APC, PM and SOS sequences, including their payloads.
  - Convert CR LF to LF. [P1] A bare CR (overwriting the current line) discards the pending line, so spinners don't flood the log.
  - Use a streaming UTF-8 decoder.
  - **WPF's regex was broken:** the character class `[@-Z\\-_]` includes `]`, so `ESC ]` matched as a 2-character sequence, and OSC payloads and BEL characters ended up in the log.
- **Writes** happen on the log thread, flushed every 1 s or every 32 KiB.
- **[P1]** Setting `logging.format = "plain" | "raw"`. Raw keeps the escape sequences.
- **[P1]** Auto-logging per session (a session flag), with a configurable log directory.

### 12.2 asciicast recording [P0]
- **Toggle:** per tab, on the focused pane.
- **File:** `%APPDATA%\UselessTerminal\recordings\<sanitized title or "recording">-yyyyMMdd-HHmmss.cast`
- **Format:** asciicast **v2**.
  - Header:
    `{"version":2,"width":<real cols>,"height":<real rows>,"timestamp":<unix>,"title":…,"env":{"TERM":"xterm-256color","SHELL":"<shell exe name>"}}`
  - Events: `[<seconds, '.' decimal separator, 6 decimals>, "o", "<data>"]`
- **Formatting MUST be culture-invariant.** WPF wrote `1,234567` on an es-ES system, which produces invalid JSON.
- **Data:** the `"o"` event text uses a streaming UTF-8 decode. Never split a multi-byte character across events.
- **Resize events:** `[t, "r", "COLSxROWS"]` [P0]. WPF recorded no resizes, and its header always said 120×30.
- **[P1]** Optional `"i"` (input) events, setting `recording.captureInput` (default `false`).
- **[P2]** Built-in playback, and asciicast v3.

### 12.3 Command-finished notification [P0]
**With shell integration (133 `C` → `D`):**
- A notification fires when the command took ≥ `notifications.commandFinished.minDurationSec` (default 10 s) **and** either the window is unfocused or the pane's tab is not active.
- **Actions:**
  - [P0] `FlashWindowEx(FLASHW_TRAY | FLASHW_TIMERNOFG)`: flash the taskbar button until the window comes to the foreground. WPF made a single `FlashWindow` call.
  - [P1] A toast: "✔ Command finished in '<tab>' after 1m 23s", or "✖ exited with 1 …". Clicking it focuses the pane.
  - Mark the tab as active (the activity dot).

**Without integration (fallback, matching WPF):**
- While the window is unfocused, count output bursts per pane.
- After at least **3** bursts followed by **2.5 s** of quiet, flash the taskbar once.
- Activating the window resets the count.

### 12.4 Bell [P0]
- Mark the tab as active.
- [P1] Visual bell: a 150 ms pane flash.
- [P1] Audible bell: default off.
- [P1] Flash the taskbar when the window is unfocused: default on.

### 12.5 Export buffer [P0]
- Ctrl+Shift+S, or the context menu: "Save Output…".
- Joins every buffer line (via `translateToString(true)`) with `\n`.
- Opens a native Save dialog with the default name `terminal-output.txt` and the filters Text, Log, and All files.
- [P2] An "export with colors" option (HTML or ANSI, using the serialize addon).

---

## 13. Theming

### 13.1 Model [P0]
**The persisted theme is a reference plus overrides:**
```json
"theme": { "preset": "AMOLED Green", "overrides": { "ansi": { "blue": "#00e676" }, "ui": { } } }
```
- **Effective theme** = the preset, then the overrides applied on top.
- WPF copied the preset's values into `settings.json`. As a result, fixing a preset in code never reached users who had already applied it. That happened during development: the AMOLED fix did nothing until the user re-selected the preset.

**A theme defines these terminal colors:**
- background, foreground, cursor, cursorAccent, selectionBackground, selectionForeground
- the **16 ANSI** colors
- muted (= brightBlack)

**UI chrome:** about 20 tokens (§13.5), derived from the theme unless overridden.

### 13.2 How semantic colors map to ANSI slots [P0]
Presets are written as **semantic roles**. They are mapped onto ANSI slots so that shells and prompts using standard colors line up predictably:

| ANSI slot | Role |
|---|---|
| red | error |
| green | command / success |
| yellow | warning |
| blue | accent (paths, links) |
| magenta | highlight |
| cyan | message / info |
| white | default text |
| brightBlack | muted |
| black | `#000000` (or the preset's own value) |

**Bright variants:**
- If the preset defines them, use those.
- **Otherwise:**
  - Dark themes: `bright = Mix(base, #ffffff, 0.15)`.
  - Light themes: `bright = Mix(base, #000000, 0.15)`.
  - brightWhite = `Mix(fg, #ffffff, 0.25)` on dark themes.
- WPF made every bright variant identical to its base. That loses the visual difference between bold/bright and normal text, which works against the user's explicit requirement that colors be "high contrast and very distinguishable".

**The typed-input color is NOT part of the theme.** It is `terminal.typedInputColor`, a personal setting. Applying a preset MUST NOT change it.
- WPF lesson: *"applying a preset must not touch it."*

### 13.3 Derivation formulas [P0]
- `Rgb(hex)`: accepts `#rgb` or `#rrggbb`. Anything invalid becomes `#000000`, and that MUST NOT throw.
- `Mix(a, b, t)`: per channel, `clamp(round_half_even(a + (b − a)·t), 0, 255)`. Output is `#RRGGBB` in uppercase.
- `Luma(hex) = (0.2126·R + 0.7152·G + 0.0722·B) / 255`, computed on the gamma-encoded values.
- `IsLight(hex) = Luma > 0.55`.
- `ContrastFg(bg) = Luma > 0.55 ? #111111 : #ffffff`.

**Dark factory:**
`Dark(bg, fg, input, muted, err, warn, cmd, msg, acc, hi, cursor, selBg, selFg, neonUi = false)`
- UI tokens copied directly:
  - `uiForeground = fg`, `uiForegroundMuted = muted`
  - `uiAccent = acc`, `uiHighlight = hi`
  - `uiSuccess = cmd`, `uiWarning = warn`, `uiError = err`
  - `uiChromeBackground = bg`
  - `uiTabForeground = fg`, `uiInputForeground = fg`
  - `uiTabSelectedBackground = acc`, `uiIcon = acc`
- Derived tokens:
  - `uiTabSelectedForeground = ContrastFg(acc)`
  - `uiInputBackground = Mix(bg, fg, 0.08)`
  - The remaining tokens are `Mix(bg, acc, t)`:

  | token | t (normal) | t (neonUi) |
  |---|---|---|
  | cardBackground | 0.12 | 0.10 |
  | cardBorder | 0.28 | 0.42 |
  | folderSelected | 0.20 | 0.24 |
  | statusBackground | 0.14 | 0.18 |
  | hoverBackground | 0.16 | 0.22 |
  | splitter | 0.40 | 0.55 |

**AMOLED factory:**
`Amoled(neon, accent, warn, err, msg, highlight)` =
`Dark(#000000, #f2f2f2, neon, #6b6b6b, err, warn, neon, msg, accent, highlight, #f2f2f2, neon, #000000, neonUi: true)`
- Lesson: *"cmd/hi/cursor were all `neon` — every role rendered as the same color, which made ls/git/prompt output in a real shell look monochrome."*
- Every AMOLED preset MUST have 9 distinct values across err, warn, cmd, msg, acc, hi, fg, muted and black.

**Light factory:**
`Light(command, accent, warn, err, highlight)`
- Terminal colors: bg `#f7f7f8`, fg `#1a1a1a`, muted `#6b7280`, msg = accent, cursor = command, selBg = command, selFg `#ffffff`.
- UI tokens:
  - uiForeground `#111827`, uiForegroundMuted `#6b7280`
  - chrome = `Mix(#f8fafc, acc, .10)`
  - card `#ffffff`
  - border = `Mix(#cbd5e1, acc, .40)`
  - folderSelected = `Mix(#ffffff, acc, .16)`
  - status = `Mix(#eef2f7, acc, .14)`
  - hover = `Mix(#ffffff, acc, .12)`
  - splitter = `Mix(#94a3b8, acc, .45)`
  - input bg `#ffffff`, input fg `#111827`
  - tab fg `#111827`, tabSelectedForeground = `ContrastFg(acc)`

**Test vectors** (computed from the formulas above):
| Preset | card | border | folderSel | status | hover | splitter | inputBg | tabSelFg |
|---|---|---|---|---|---|---|---|---|
| Default | #0D1B1F | #1E4047 | #152E33 | #0F2024 | #112529 | #2B5C66 | #141414 | #111111 |
| Dracula | #3A374D | #52476D | #463F5D | #3D3951 | #403B55 | #645484 | #393A45 | #111111 |
| AMOLED Green | #00170C | #006132 | #00371C | #002915 | #00331A | #007F41 | #131313 | #111111 |
| Light (chrome #DFEEF1) | #ffffff | #7AB4C0 | #D6EBED | #CDE2E8 | #E0F0F2 | #5195A6 | #ffffff | #ffffff |

### 13.4 Presets [P0]
There are 31 presets. The full table is in **Appendix B**. Their names, in display order:
- Default, Dracula, Solarized Dark, Monokai, Nord, Catppuccin Mocha, One Dark, Gruvbox Dark, Tokyo Night, Ayu Dark, Vesper
- AMOLED Green, Red, Purple Neon, Cyan, Orange, Pink, Blue, Gold, Matrix, Ice
- Light, Light Green, Light Red, Light Purple Neon, Light Cyan, Light Orange, Light Pink, Light Blue, Light Gold, Light Ice

**[P1] Further dark presets**, designed but not yet implemented:
- Rosé Pine, Kanagawa, Night Owl, Poimandres, Everforest Dark, Iceberg Dark.
- Take their palettes from the official upstream themes, and map them through `Dark()`.

**[P1] User themes:** `%APPDATA%\UselessTerminal\themes\*.json`, using the same schema as a preset, loaded at startup and when the folder changes.

### 13.5 UI tokens [P0]
Expose these as CSS custom properties on `:root`.

**Color tokens:**
- `--ui-foreground`, `--ui-foreground-muted`
- `--ui-accent`, `--ui-accent-dim` (the accent at 14% alpha, **derived**)
- `--ui-highlight`, `--ui-success`, `--ui-warning`, `--ui-error`
- `--ui-chrome-bg`, `--ui-card-bg`, `--ui-card-border`, `--ui-folder-selected`
- `--ui-tab-fg`, `--ui-tab-selected-bg`, `--ui-tab-selected-fg`
- `--ui-status-bg`, `--ui-icon`
- `--ui-input-bg`, `--ui-input-fg`
- `--ui-hover-bg`, `--ui-splitter`
- `--ui-terminal-bg`

**Font tokens:**
- `--ui-font-family` (default `Segoe UI`)
- `--ui-font-size`
- `--ui-font-size-small` = size − 2
- `--ui-font-size-title` = size + 2
- `--ui-font-size-tab` = size − 3
- `--ui-font-weight`

**UI scale:**
- A single multiplier applied to the root font size, or to CSS `zoom` on the chrome only, never on terminals.
- Range 0.75–2.0, snapped to 0.05.
- Ctrl+mouse wheel over the chrome (not over a terminal) changes it by ±0.05.
- WPF lesson: never scale the chrome with a raster transform, because it *"makes labels/icons fuzzy."*

**Theme switches apply immediately:** update the CSS variables, call `term.options.theme = …` on every pane, then `refresh(0, rows−1)`.

---

## 14. Settings

### 14.1 File and behavior [P0]
- **File:** `%APPDATA%\UselessTerminal\settings.json`, using camelCase keys with `schemaVersion: 3`.
- **Writes:** atomic, debounced by 500 ms.
- **[P1]** Watch the file and hot-reload it on external edits. Show invalid JSON as a non-blocking error, and keep the last valid settings in use.
- **On a parse error:**
  - Keep the file untouched.
  - Use the defaults in memory.
  - Show a banner offering "Open file" or "Reset (backup created)".
  - WPF silently reset everything, and the next save overwrote the user's file.
- **Validation and clamping** happen in one place, at load time and on every patch.

### 14.2 Schema (defaults shown)
```jsonc
{
  "schemaVersion": 3,
  "terminal": {
    "fontFamily": "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace",
    "fontSize": 14,                 // 8–32
    "fontWeight": 400,              // 100–900 (UI slider 300–700 step 50); bold = min(900, w+250)
    "lineHeight": 1.0,              // [P1] 0.8–2.0
    "letterSpacing": 0,             // [P1]
    "cursorStyle": "bar",           // bar | block | underline
    "cursorBlink": true,
    "scrollback": 10000,            // 0–200000
    "renderer": "webgl",            // [P1] webgl | dom
    "rendererRepair": "auto",       // [P1] auto | on | off  (§5.3)
    "typedInputColor": "#ffffff",   // personal, NOT part of the theme
    "overridePsReadLineColors": true,
    "copyOnSelect": false,          // [P1]
    "clearSelectionOnCopy": true,
    "trimTrailingWhitespaceOnCopy": true,
    "rightClick": "menu",           // [P1] menu | paste | copyPaste
    "multiLinePasteWarning": "auto",// [P1] auto | always | never
    "pasteImages": "passThroughCtrlV", // passThroughCtrlV | inlinePreview
    "osc52": "write",               // [P1] write | readwrite | off
    "zoomScope": "global",          // global | pane
    "closeOnExit": "never",         // [P1] never | graceful | always
    "refreshEnvironment": true,     // [P1] §4.3
    "conptyImplementation": "auto", // [P1] auto | bundled | system
    "bell": { "visual": true, "audible": false, "flashTaskbar": true },
    "backgroundImage": { "path": "", "opacity": 0.52 },
    "crt": false,
    "minimap": false
  },
  "theme": { "preset": "Default", "overrides": {} },
  "ui": {
    "fontFamily": "Segoe UI",
    "fontSize": 13,                 // 10–22
    "fontWeight": 400,              // 300–700
    "scale": 1.0,                   // 0.75–2.0, step 0.05
    "backdrop": "none"              // none | mica | acrylic
  },
  "shells": { "defaultProfile": "auto" },
  "startup": { "mode": "restoreLastSession", "workspaceId": null }, // [P1] restoreLastSession | workspace | defaultTab
  "processes": { "closeConfirm": "whenRunning", "killConsoleTreeOnClose": true },
  "notifications": { "commandFinished": { "enabled": true, "minDurationSec": 10, "toast": true } },
  "ssh": { "connectionReuse": "auto" },
  "drop": { "defaultAction": "copy" },  // [P1] copy | paste
  "links": { "editorCommand": "code --goto \"{file}:{line}\"" },
  "logging": { "format": "plain", "directory": "" },
  "recording": { "captureInput": false },
  "quake": { "hotkey": "Win+Backquote", "dropdown": false, "heightPercent": 50, "hideOnBlur": false }
}
```
- WPF's `UiSharpness` setting (WPF text-hinting modes) has no webview equivalent. Drop it.
- `ui.backdrop` replaces WPF's `WindowBackdrop`.

### 14.3 Settings UI [P0]
- **Layout:** a full panel in the main webview, either a modal or a tab-like page. It has a sidebar of sections.
- **[P1] Live preview:** changes apply immediately. Cancel reverts them. Save persists them.
  - WPF had no preview; edits only showed up after Save.
- **Sections and contents:**
  1. **Terminal font:**
     - Family: a picker filtered to **monospace** fonts [P1], enumerated through DirectWrite in the backend with the `IsMonospacedFont` flag. Free text is also allowed for a CSS font stack.
     - Size (8–32), weight (300–700, step 50), line height.
     - The UI scale slider (75–200%), with the hint "Ctrl+scroll over tabs, session panel, or status bar".
  2. **Interface:**
     - UI font, size and weight.
     - A grid of the 20 UI color tokens, each with a label, a swatch and a hex field.
     - Cursor style and blink.
     - Scrollback.
     - Backdrop.
  3. **Shell background:** image path (Browse / Clear) and an opacity slider (0–100%).
  4. **Prompt & output:**
     - The theme preset dropdown. It MUST **show the currently active preset**; WPF always showed "Custom".
     - A 13-row color grid: background, default text, typed input, muted, errors, warnings, commands & success, info, paths & links, highlights, cursor, selection background, selection foreground.
     - [P1] All 16 ANSI slots, behind an "Advanced" toggle.
     - [P1] A live sample pane, rendering a fixed ANSI test string: `ls --color` style output, a git-status-like block, a prompt, and errors and warnings.
  5. **[P1] Behavior:** copy/paste options, right-click, bell, close-on-exit, notifications, startup mode, default profile.
  6. **[P1] Keybindings** (§15).
  7. **[P1] SSH & drops.**
  8. **About:** version, credits (Developer: Unnamed10110), update check.
- **Color fields:**
  - The hex input accepts `#rgb` or `#rrggbb`. It is normalized to lowercase `#rrggbb` when saved.
  - The swatch opens a color picker that includes alpha where the setting allows it.
- **Reset defaults:** resets the in-memory copy only. Nothing is saved until the user clicks Save.

---

## 15. Keybindings

### 15.1 File [P0]
- **Location:** `%APPDATA%\UselessTerminal\keybindings.json`
  ```json
  { "schemaVersion": 3, "bindings": { "<action>": "<chord>" | ["<chord>", …] | null } }
  ```
- **Missing actions** fall back to their defaults.
- **`null` unbinds an action,** so the key passes through to the shell.
- **The app writes this file** when the user changes a binding in the UI. WPF never wrote it.
- **[P1]** Hot-reload the file when it changes.
- **Chord syntax:** `Ctrl+Shift+Alt+<Key>`.
  - Key names: `A`–`Z`, `0`–`9`, `F1`–`F24`, `Tab`, `Enter`, `Esc`, `Space`, `Backspace`, `Delete`, `Insert`, `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`, `Numpad0`–`Numpad9`, `Comma`, `Period`, `Minus`, `Equal`, `Backquote`, `Slash`, `Backslash`, `BracketLeft`, `BracketRight`, `Semicolon`, `Quote`, and the special `Arrow` (meaning any arrow key).
  - **Legacy WPF names** are mapped on import: `D1` → `1`, `OemComma` → `Comma`, `OemMinus` → `Minus`, `OemPlus` → `Equal`, `Oem3` → `Backquote`, `NumPad1` → `Numpad1`, and so on.
- **Matching rules:**
  - Letter keys match on `KeyboardEvent.key`, case-insensitively, so the key labelled "T" on the current layout is "T".
  - Digit, numpad, function, navigation and punctuation keys match on `KeyboardEvent.code`.
  - Modifiers must match **exactly**.
  - An event where AltGr is active never matches (§5.5).
- **The app owns no `Win` chords inside the webview.** `Win+…` combinations exist only as global hotkeys.

### 15.2 Defaults [P0]
| Action | Default | Notes |
|---|---|---|
| `newTab` | Ctrl+T | |
| `closePane` | Ctrl+W | closes the focused pane; closes the tab when it is the last pane; no-op on a pinned single-pane tab |
| `togglePanel` | Ctrl+B | |
| `toggleBrowser` | Ctrl+Shift+B | WPF hard-coded this one; here it is configurable |
| `settings` | Ctrl+Comma | |
| `nextTab` / `prevTab` | Ctrl+Tab / Ctrl+Shift+Tab | |
| `selectTab1..9` | Ctrl+1..9 | consumed only if that tab exists |
| `selectTabNumpad0..9` | Ctrl+Alt+Numpad0..9 | Numpad0 selects tab 10 |
| `newSession` | Ctrl+Shift+N | |
| `duplicateTab` | Ctrl+Shift+D | |
| `commandPalette` | Ctrl+Shift+P | |
| `quickConnect` | Ctrl+Shift+O | |
| `movePaneFocus` | Ctrl+Shift+Arrow | consumed only when a move is possible |
| `prevCommand` / `nextCommand` | Ctrl+Alt+Up / Ctrl+Alt+Down | |
| `search` | Ctrl+Shift+F | |
| `exportBuffer` | Ctrl+Shift+S | |
| `copy` | Ctrl+Shift+C, Ctrl+Insert | plus Ctrl+C when there is a selection |
| `paste` | Ctrl+V, Ctrl+Shift+V, Shift+Insert | |
| `splitRight` / `splitDown` [P1] | Alt+Shift+Equal / Alt+Shift+Minus | |
| `zoomIn` / `zoomOut` / `zoomReset` [P1] | Ctrl+Equal / Ctrl+Minus / Ctrl+0 | |
| `scrollPageUp` / `scrollPageDown` [P1] | Shift+PageUp / Shift+PageDown | |
| `quake` (global) | Win+Backquote | the key label follows the keyboard layout |

**Conflict notice** [P1]:
- Ship a built-in alternative keymap, **"Shell-safe (Windows Terminal style)"**, where:
  - New tab is Ctrl+Shift+T.
  - Close pane is Ctrl+Shift+W.
  - Toggle the sessions panel is Ctrl+Shift+E.
- The defaults above take **Ctrl+B** (tmux's prefix key, and readline's back-one-character), **Ctrl+W** (readline/zsh delete-previous-word) and **Ctrl+T** (readline transpose-characters) away from the shell.
- Show a one-time tip about this the first time a tmux session is detected.

### 15.3 Keybindings UI [P1]
- A searchable list of actions.
- Click an action, then press a chord to record it.
- Conflicts are highlighted.
- Each action has "Reset" and "Unbind".

---

## 16. Persistence

### 16.1 Files
| Path | Content |
|---|---|
| `%APPDATA%\UselessTerminal\settings.json` | settings (§14) |
| `…\sessions.json` | folders, sessions (and optionally snippets) (§8.4) |
| `…\snippets.json` | snippets (if not merged into sessions.json) |
| `…\workspaces.json` | workspaces (§16.4) |
| `…\keybindings.json` | keybindings (§15) |
| `…\windowstate.json` | window and tab state (§16.3) |
| `…\themes\` | user themes [P1] |
| `…\logs\`, `…\recordings\` | §12 |
| `%LOCALAPPDATA%\UselessTerminal\WebView2` | main webview profile |
| `%LOCALAPPDATA%\UselessTerminal\WebView2Browser` | browser panel profile (separate) |
| `%LOCALAPPDATA%\UselessTerminal\ssh-mux` | ControlPath directory |
| `%LOCALAPPDATA%\UselessTerminal\icon-cache` | extracted shell icons |

### 16.2 Write discipline [P0]
- **Atomic writes:**
  1. Serialize to `<file>.tmp` in the same directory.
  2. Flush it.
  3. `ReplaceFileW(target, tmp)` if the target exists. Otherwise `MoveFileExW(tmp, target, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)`.
- **Debounce** per file: 300–500 ms.
- **Flush all pending writes** synchronously on exit, on `WM_QUERYENDSESSION` / `WM_ENDSESSION` (logoff or shutdown), and when hiding to the tray.
- WPF wrote atomically only for `windowstate.json`. Every other file used plain `File.WriteAllText`, and errors were swallowed.
- **Single instance** [P0]:
  - Use `tauri-plugin-single-instance`.
  - A second launch forwards its arguments (`--cwd <dir>`, `--session <name|id>`, `--workspace <name>`, `-- <command…>`) to the running instance, which opens a new tab with them.
  - This also stops two instances from overwriting each other's files.

### 16.3 Window state [P0]
- **Contents:**
  - Bounds and maximized state.
  - Sidebar open/closed and width; browser panel open/closed and width [P1].
  - The active tab index.
  - For each tab:
    - title, `titleLocked`, command, `sessionId`
    - `cwd` — the **local** live cwd, but only if it exists locally; otherwise the original directory
    - starting command, color, pinned, group, read-only
    - [P1] the pane layout tree, and each pane's profile and cwd
- **Save triggers:** an autosave every 15 s (only when something changed), on close, on logoff or shutdown, and when hiding to the tray.
  - WPF lesson: the autosave exists *"so a force-kill or crash still leaves a usable session."*
- **Guards:**
  - Never save before the restore has finished.
  - Once window close has started, block any autosave that was already queued; otherwise an **empty** tab list would be saved.
  - Closing the last tab saves `tabs: []`, so the next launch opens one default tab.
- **Bounds capture:**
  - Record bounds only while the window is in the Normal state.
  - When maximized, save the restore bounds.
  - Never save NaN or 0.
  - WPF lesson: reading bounds *"on a hidden/minimised window can yield NaN or 0; those would overwrite a perfectly good saved state."*
- **Restore:**
  1. Clamp the saved bounds to the **work areas of the connected monitors**. This is better than WPF, which used the virtual screen.
     - At least 120 px of the window must stay visible horizontally.
     - The title bar must be reachable.
     - A window that is entirely off-screen is centred on the primary monitor.
     - A window larger than its monitor is shrunk to the monitor size minus 80 px.
  2. Restore each tab inside its own try/catch, so one bad tab doesn't stop the others.
     - WPF lesson: *"A bad command saved from a previous version shouldn't drop the rest."*
  3. If nothing could be restored, open one default tab.
- **[P1] Lazy start of restored tabs.** Only the active tab's PTY starts immediately. Other restored tabs start when first shown, or 2 s after startup in the background, whichever comes first. Setting: `startup.lazyRestore = true`.
  - WPF started every restored tab at once.

### 16.4 Workspaces [P0, improved]
- **Schema:**
  `{ id, name, tabs: [{ sessionId?, title, command, cwd?, startingCommand?, color?, layout? [P1] }] }`
- **Saving:**
  - "Save Current Tabs as Workspace" asks for a name, prefilled "My Workspace".
  - Each tab's **sessionId is stored** when the tab came from a session. WPF never stored it.
  - The tab's cwd is its live local cwd.
- **Opening:** asks "Replace current tabs / Add to window" [P1]. WPF always appended.
- **Session-backed tabs** apply the session's environment, theme, font and `sessionId`. WPF applied none of these.
- **[P1] Management:** rename and delete workspaces, from the palette or from Settings.
- **[P1] Startup:** open a chosen workspace at launch (`startup.mode = "workspace"`).

### 16.5 Migration from the WPF version [P1, strongly recommended]
- **Trigger:** on first run, any file without `schemaVersion` (or with PascalCase keys) is treated as legacy.
- **Process:**
  1. Copy it to `<name>.legacy-bak.json`.
  2. Convert it in memory.
  3. Write it back in the v3 format.
- **Legacy formats** are listed in Appendix C.
- **Mapping:**
  - The old flat color keys become a `theme` with **`preset: "Custom"`**, with every legacy color placed in `overrides`. The old files don't record which preset was used.
  - The old `ColorInput` becomes `terminal.typedInputColor`.
  - Keybinding key names are translated (§15.1).

---

## 17. Browser panel [P0]

### 17.1 Hosting and isolation
- **Hosting:** a separate **child webview**, created with Tauri v2 multi-webview, which needs the `unstable` feature (verify).
  - Its bounds are kept in sync with the panel element's rectangle: a `ResizeObserver` on the panel element calls `browser_set_bounds`.
  - Fallback: a borderless owned window, snapped to the right edge of the main window.
- **The browser webview MUST have no Tauri IPC capabilities at all.** No commands and no events. It loads arbitrary remote sites, and an IPC bridge would let them write to your terminals.
- **Separate data directory:** `…\WebView2Browser`. Logins and cookies persist there, independently of the terminal webview.
- **Settings:**
  - Context menus on, DevTools on.
  - Autofill and password save on. This is the user's own browser profile.
  - Status bar off.
- **New-window requests** (`window.open`, `target=_blank`) navigate in the same view.
- **Lazy creation:** create the webview the first time the panel is opened. WPF created it at startup and loaded chatgpt.com even when the panel was never opened.
- **Airspace:** the child webview draws above the main webview, so in-page overlays such as the command palette and dialogs can't appear on top of it. When a modal overlay opens, hide the browser webview. [P2] Show a static screenshot of it in its place.

### 17.2 User interface
- **Navigation bar:** Back, Forward, Refresh, then the address bar.
- **Quick links:**

  | Name | URL |
  |---|---|
  | ChatGPT | https://chatgpt.com |
  | DeepSeek | https://chat.deepseek.com |
  | Claude | https://claude.ai |
  | Gemini | https://gemini.google.com |
  | Copilot | https://copilot.microsoft.com |
  | Perplexity | https://www.perplexity.ai |
  | Grok | https://grok.com |

  [P1] The list is editable in Settings.
- **Initial page:** the last URL visited [P1]; otherwise https://chatgpt.com.
- **Address bar:**
  - Empty input is ignored.
  - Text without `://` that contains a `.` and no space gets `https://` prepended.
  - Any other text becomes `https://www.google.com/search?q=<encoded>`. [P1] The search engine is configurable.
- **Toggle:** Ctrl+Shift+B, the globe button, or the palette.
- **[P1] "Send selection to browser":** copy the terminal selection, focus the browser, and paste it into the page's focused input. This is clipboard-based only; no page-script injection.

---

## 18. Security requirements [P0]

### 18.1 IPC capabilities
- **Main webview:** gets only the commands it needs, scoped by window/webview label.
- **Browser webview:** gets none (§17.1).
- **CSP for the main webview:**
  `default-src 'self'; img-src 'self' data: blob: utasset: uticon:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src ipc: http://ipc.localhost`
  Adjust it to whatever Tauri v2 actually requires (verify). The main webview never loads remote content.

### 18.2 Opening links and files
- **`open_external` allowlist:** `http`, `https`, `mailto`. Any other scheme (`file`, `ms-settings`, custom protocols) requires a confirmation dialog that shows the full URL.
  - WPF shell-executed any URI it was given.
- **`open_path`:** only paths that exist, and only in explorer or the editor. Never run an executable directly from a clicked link: for `.exe`, `.bat`, `.cmd`, `.ps1`, `.vbs` and `.lnk`, open the parent folder instead.

### 18.3 Paste, clipboard, files and credentials
- **Paste:** sanitize as described in §5.6.
- **OSC 52:** clipboard read is off by default.
- **Asset protocols:** `utasset://` serves only the configured background image. `uticon://` serves only the icon cache.
- **SSH:**
  - Never store passwords.
  - Helper commands always use `BatchMode=yes`, so they never prompt.
  - The ControlPath directory is user-only (ACL).
- **Remote `rm -rf`:** guarded against root and home directories (§11.4).
- **Logs** never capture keyboard input. Recording input is opt-in (§12.2).

### 18.4 Elevation and updates
- **Elevation:** never self-elevate. Elevated sessions open in separate consoles (§4.9).
- **[P1] Updates:** signed (the Tauri updater with minisign keys) and delivered over HTTPS.

---

## 19. Performance requirements

### 19.1 Budgets [P0]
Reference machine: 4 cores, SSD, WebView2 runtime installed and warm.

| Metric | Budget |
|---|---|
| Process start → first painted frame | ≤ 400 ms p50, ≤ 800 ms p95 |
| Ready → PTY spawned (first tab) | ≤ 50 ms |
| New tab (app overhead, excluding the shell's own startup) | ≤ 50 ms |
| Tab switch | ≤ 1 frame (16 ms); no webview creation |
| Keystroke → `WriteFile` (app side) | ≤ 2 ms p99 |
| PTY read → painted (idle system) | ≤ 1 frame p99 |
| Sustained throughput (`type`/`cat` of a 200 MB text file; pwsh and WSL) | ≥ 80% of Windows Terminal on the same machine, and never below 10 MB/s. The UI stays interactive throughout (input echo ≤ 100 ms). |
| Ctrl+C during a flood | output stops within ≤ 300 ms |
| Idle CPU, 10 tabs, no output | < 0.5% of one core. No backend timers faster than 1 Hz; no JS intervals other than the cursor blink. |
| Memory | ≤ 200 MB with 1 tab. ≤ +20 MB per extra pane at 10k scrollback and 120 columns (the xterm buffer is about 12 B/cell, so ≈ 14 MB at full scrollback). |
| Resize while dragging the window | no frame drops above 33 ms; at most one PTY resize per 16 ms |

### 19.2 Rules [P0]
- One webview for all terminals (§3.1).
- Binary IPC frames, credit-based flow control, and input sent separately from output (§3.4, §3.5).
- **No synchronous work on the UI or IPC threads**, specifically:
  - process snapshots
  - registry scans
  - `wsl.exe` calls
  - icon extraction
  - file I/O
  - git HEAD reads
- **Event-driven state.** No polling timers for:
  - the status bar
  - live-session dots
  - git, except a 3 s poll for the focused pane when it has no shell integration
- **Hot paths stay out of the frontend framework's reactivity.** Output writes, decorations and the minimap never trigger component re-renders.
- **Settings patches are deltas.** Never re-send unchanged heavy data, such as images.
- **WebGL context budget:** an LRU of at most 8 (§5.3).
- **Lazy creation:**
  - the browser webview
  - restored tabs [P1]
  - the icon cache
  - the shell detection cache

### 19.3 Measurement [P0]
Ship a hidden "Diagnostics" page (opened from the palette with `Developer: Diagnostics`) showing:
- per-pane bytes/s, unacked bytes, frames/s, average frame size;
- the renderer in use (WebGL or DOM) and the number of live WebGL contexts;
- memory, from `performance.memory` and the backend process working set.

Include an automated benchmark script, `bench/throughput.ps1`, that runs:
- `type` of a 200 MB file;
- `seq 1 5000000` in WSL.

It reports wall time and the 99th-percentile input latency, sampled with a background keystroke injector.

---

## 20. Packaging and release

### 20.1 Installers [P0]
- **Bundler:** the Tauri bundler.
  - **NSIS** installer: per-user install, no admin needed. This is the recommended default.
  - **MSI**: per-machine install, for enterprise use.
  - **[P1] Portable ZIP.**
- **MSI `upgradeCode`:** MUST be `{E8D4F9B2-6C1A-4F70-9E3D-2B7A8C5D1E0F}`, the same as the WPF MSI and the old Inno Setup AppId. Then installing the Rust version upgrades the WPF one cleanly (verify the Tauri WiX config key).
- **MSI ProductVersion** must be numeric `major.minor.build`. Strip any semver suffix (such as `-beta1`) for the MSI version only; keep it in file names.
- **Executable name:** `UselessTerminal.exe`, so existing shortcuts and pins keep working.
- **WebView2 runtime:** `webviewInstallMode = downloadBootstrapper` by default. [P1] An offline-installer variant.
- **Shortcuts:** Start Menu and Desktop. [P1] The desktop shortcut is optional, as an installer checkbox.
- **"Launch after install"** checkbox.
- **[P1] Explorer context menu:** "Open in Useless Terminal" (it uses `--cwd "%V"`).
- **Code signing:** [P1] Authenticode, if a certificate is available.
- **Lesson from the WPF build:** if a script ever calls a globally installed `wix` tool directly, pin its version. `dotnet tool update -g wix` jumped to v7, which requires a paid EULA (the OSMF) and silently broke extension loading (WIX0144). Tauri's bundler fetches its own WiX 3, so this only matters for custom scripts.

### 20.2 Release [P0]
- **Script** `scripts/release.ps1 -Tag vX.Y.Z [-Draft] [-Prerelease] [-Notes …] [-SkipBuild]`:
  1. Validate the tag against `^v?\d+\.\d+\.\d+`.
  2. Run `tauri build` for x64 and arm64.
  3. Create an annotated tag, if it doesn't already exist, and push it.
  4. Run `gh release create <tag> <msi> <nsis.exe> <zip> --title <tag> [--generate-notes | --notes]`.
- **[P1] GitHub Actions** using `tauri-action` with a matrix (x64 and arm64), plus the updater's `latest.json`.

---

## 21. Testing and acceptance

### 21.1 Unit tests (Rust) [P0]
- **SSH:** the argv parser, including attached values (`-p2222`) and IPv6.
- **SSH config:** the parser, including `=` separators, `Include` and multi-pattern `Host` lines.
- **Quick connect:** the grammar.
- **OSC 7 normalization:** every form listed in §6.3.
- **OSC 133:** the turn state machine.
- **Environment:**
  - parsing of the session environment string;
  - environment-block sorting and precedence (`TERM` is always present; session values win).
- **`GetFullCommand`**, including quoting and the WT-import case.
- **Shell-kind detection** on sample command lines.
- **Theme derivation** against the test vectors in §13.3. Also check that every AMOLED preset has 9 distinct role colors.
- **Legacy migrations**, using fixture files (Appendix C).
- **Atomic writes:** a write interrupted part-way leaves either the old file or the new file, never a partial one.
- **asciicast:** formatting under an es-ES locale, `"r"` events, and UTF-8 split across reads.
- **Log stripper:** OSC, DCS, CSI and split sequences.

### 21.2 Integration tests (Rust, real ConPTY) [P0]
- Spawn `cmd /c echo hi` → the output contains `hi`, the exit code is 0, and `pane:exited` fires.
- Spawn `cmd /c exit 3` → the exit code is 3.
- Resize → `mode con` reports the new size.
- Backpressure: flood the output while acks are withheld → the reader blocks once unacked > HIGH, and memory stays bounded.
- Process-tree policy: console grandchildren die on close; `notepad` survives (§4.8).
- The environment block contains `TERM=xterm-256color` unless the session overrides it.

### 21.3 Manual and E2E acceptance checklist [P0]
Run each item on Windows 10 21H2 and Windows 11, at 100% and 150% DPI.

- **PowerShell 7 with oh-my-posh and PSReadLine predictions** (`ListView`):
  - Type, and navigate the history list.
  - Run `cls` 10 times.
  - Resize during the prediction list.
  - Expected: **no ghost characters** (WebGL on), the status bar shows the exit code, and Ctrl+Alt+Up/Down jump between prompts.
- **cmd:**
  - The prompt markers work, the cwd updates the status bar, and `cls` is clean.
  - Multi-line paste shows a warning, and no `[200~` garbage appears.
- **WSL Ubuntu:** htop, vim (with the mouse), tmux (Ctrl+B works when the keymap is "shell-safe"), `ls --color`, emoji, CJK, and Nerd Font icons.
- **Git Bash:**
  - `cd` updates the cwd.
  - Dropping a file copies it into the cwd.
- **SSH to Linux:**
  - **Colors are present** (`ls --color`, `git diff`), `htop` renders.
  - Dropping a file uploads it to the remote cwd, conflict dialog included.
  - Nested ssh works.
- **es-ES layout:** AltGr+2 (`@`), AltGr+3 (`#`) and AltGr+º (`\`) type correctly in every shell. Win+Ñ toggles Quake mode.
- **Flood:** `type` a 200 MB file → the UI stays responsive, and Ctrl+C stops it within 300 ms.
- **Clipboard:**
  - Ctrl+C with a selection copies; without a selection it sends `^C`.
  - Ctrl+V pastes text exactly once.
  - With an image on the clipboard, Claude Code (or a similar CLI) receives the Ctrl+V and can paste the image.
- **ShareX:** scrolling capture in Windows-message mode captures all of the scrollback, with ShareX **not** elevated.
- **"Run as administrator"** on a session opens a separate elevated console. The app itself stays non-elevated.
- **Restore:**
  - Kill the app process → relaunch → the tabs come back with their cwds.
  - Disconnect a monitor → relaunch → the window is on-screen.
- **Themes:**
  - Switching presets updates every pane live.
  - The typed-input color is unchanged.
  - A preset fix shipped in a new version shows up without re-selecting the preset (§13.1).
- **Browser panel:**
  - Logins persist across restarts.
  - The browser page cannot call app IPC: verify in DevTools that `window.__TAURI__` and `__TAURI_INTERNALS__` are absent or inert.

---

## 22. Implementation milestones

| Milestone | Scope | Exit criteria |
|---|---|---|
| **M0** | Tauri skeleton; `ut-pty` with full ConPTY spawn/exit/close; one pane; binary channel; flow control; diagnostics page; throughput bench | §21.2 passes, and the §19 throughput and latency budgets are met with a single pane |
| **M1** | Tabs, fixed 4-pane splits, keybindings, status bar, settings + themes + presets, persistence + atomic writes, window state restore, single instance, tray | Parity for daily use |
| **M2** | `ut-vt-scan`; OSC 7/133 and the others; PowerShell/cmd/bash/zsh injection; exit codes; command navigation; git branch; notifications; typed-input color | The PowerShell and cmd checklist items pass |
| **M3** | Sessions sidebar (tree, drag-and-drop, edit dialog, search, context menus), snippets, workspaces, WT and SSH-config import, import/export, migration from WPF | The user's existing `%APPDATA%` data migrates losslessly |
| **M4** | SSH target resolution, quick connect, file drops (local and remote), conflict dialog | The SSH and drop checklist items pass |
| **M5** | Browser panel (isolated), Quake mode, command palette, logging, asciicast, search with counter, CRT, minimap, ShareX bridge | The full §21.3 checklist passes |
| **M6** | Installers (NSIS + MSI with the existing upgrade code), release script, updater, signing; then the [P1] items | Release |

---

## 23. Hard-won compatibility lessons (preserve all of them)
1. **Spawn the PTY only after the terminal has real, stable dimensions** and its fonts have loaded. Otherwise pwsh and oh-my-posh cache a wrong width and PSReadLine draws garbage.
2. **Always resume a `CREATE_SUSPENDED` shell on every code path.** A suspended shell hangs silently.
3. **Drop the job object rather than fail the spawn** when job assignment fails.
4. **Call `ClosePseudoConsole` while the reader is still draining.** Detect exit via the process handle, not via pipe EOF.
5. **A write after the pane is disposed is a silent no-op.**
6. **Always set `TERM` (and `COLORTERM`).** Without them, remote SSH shells disable color entirely.
7. **Inject PowerShell integration through `-NoExit -EncodedCommand`** (UTF-16LE base64), and wrap the prompt *after* the user's profile has run. Re-wrap it on `PowerShell.OnIdle`, because oh-my-posh, starship and posh-git reassign `prompt`.
8. **Return OSC 7 from the PowerShell prompt string itself**, not from a separate `Write`: that avoids timing races.
9. **PSReadLine colors must be 24-bit VT sequences.** `#RRGGBB` fails on older PSReadLine builds. Apply them late, from the prompt or OnIdle, because PSReadLine isn't loaded yet when `-EncodedCommand` runs.
10. **cmd can only do A/B markers and a bare D.** In `$e\`, the `\` makes it ST: cmd has no BEL escape.
11. **A bare 133;D means "no exit code".** Keep the previous status-bar value. Tolerate `D;0;extra`.
12. **Handle paste in exactly one place.** WebView2 fires both keydown and a native paste event, which leads to a double paste.
13. **Snap font size to `round(px·dpr)/dpr`**, or glyphs come out blurry.
14. **In-place redraws (PSReadLine, `cls`, leaving the alternate screen) can leave ghost cells.** Test them; keep the repair strategy available (§5.3).
15. **Use an opaque terminal background unless a background image is active.** Transparency combined with erase operations leaves ghost pixels.
16. **A CRT glow must be neutral.** A colored glow overrides every theme.
17. **Presets must give every semantic role a distinct color.** Collapsed roles make shell output monochrome.
18. **Theme presets must never change the user's typed-input color.**
19. **Persist the theme as a reference plus overrides**, never as copied values.
20. **Never elevate the whole app.** UIPI then breaks ShareX, automation tools, and drag-and-drop from Explorer.
21. **Handle `WM_VSCROLL`** for ShareX scrolling capture.
22. **Never hide the window on minimize.** It breaks DWM thumbnails and leaves blank surfaces on restore.
23. **Never save window bounds read from a hidden or minimized window** (they come back NaN or 0). Never save state before restore has finished, or after close has started.
24. **Restore each tab independently.** One bad tab must not drop the others.
25. **The window must stay resizable at its edges** even when the webview covers the client area.
26. **Never scale the UI with raster transforms**, because it makes text fuzzy.
27. **A contrast-computed dark foreground on a tinted selected tab was unreadable.** Use the light tab foreground.
28. **Format numbers in files culture-invariantly.** The user's locale is es-ES, which writes decimal commas.
29. **Test with paths that contain spaces.** The user's home directory and repositories are under `OneDrive - BEPSA DEL PARAGUAY SAECA`.
30. **Win32 `wsl.exe --list` writes UTF-16.** Prefer the registry.
31. **Pin the versions of external build tools.** WiX v7 silently broke a v5-based build.

---

## 24. Known defects in the WPF implementation — do NOT replicate
| # | Defect | Required behavior |
|---|---|---|
| 1 | One WebView2 per pane, each creating its own environment | One webview for the whole UI (§3.1) |
| 2 | base64 + `ExecuteScript` output path; the high-water mark ignored xterm's internal queue | Binary channel with credit-based acks (§3.5) |
| 3 | WebGL disabled, so the slow DOM renderer ran | WebGL with an LRU budget and repair fallback (§5.3) |
| 4 | Broadcast stripped CR/LF and appended `\r` to every chunk, so each keystroke executed | Forward raw bytes (§7.4) |
| 5 | Exit detected only by pipe EOF; exit code never shown | Process-handle watcher plus exit code UI (§4.7) |
| 6 | `KILL_ON_JOB_CLOSE` killed GUI apps launched from the shell | Kill console processes only (§4.8) |
| 7 | Logger regex `[@-Z\\-_]` swallowed `ESC ]`, so OSC payloads and BEL leaked into logs | VT-parser-based stripping (§12.1) |
| 8 | asciicast timestamps culture-formatted (`1,234567`); header hard-coded 120×30; no resize events | Invariant formatting, real size, `"r"` events (§12.2) |
| 9 | UTF-8 decoded per read in the logger and recorder (U+FFFD at chunk edges) | Streaming decoders |
| 10 | Every zoom step re-sent the full settings payload to every pane, re-reading and base64-encoding the background image (up to 15 MiB) | Deltas only, image served by URL (§5.9, §5.12) |
| 11 | `Ui.AccentDim` (selected-tab tint) never followed the theme accent | Derive it (§13.5) |
| 12 | Bright ANSI variants identical to normal ones | Derived brights (§13.2) |
| 13 | Theme stored as copied values, so preset fixes never reached users | Preset reference plus overrides (§13.1) |
| 14 | Settings dialog always showed "Custom"; no live preview | Show the active preset; live preview (§14.3) |
| 15 | Settings, sessions, workspaces, snippets and keybindings written non-atomically, errors swallowed; a parse error reset and then overwrote the user's file | Atomic writes, backups, banners (§16.2, §14.1) |
| 16 | Empty session list re-seeded on every start, so deleted sessions came back and a corrupt file was overwritten | Seed on first run only (§8.2) |
| 17 | Session edit dialog reset any color not in the swatch list to green | Keep it; offer a custom color (§8.3) |
| 18 | `Clone()` (duplicate session) dropped the theme, font and env | Copy every field |
| 19 | `GetFullCommand` quoted entire WT-imported command lines | §8.1 |
| 20 | SSH config import kept only 4 keys; `Host a b` became one session named "a b" | `ssh <alias>`, one session per pattern (§8.7) |
| 21 | Quick connect broke on IPv6 | §10.2 |
| 22 | ControlMaster injected into Win32-OpenSSH, which (historically) doesn't support it | Capability-gated (§10.5) |
| 23 | Remote `test -e` / `rm -rf` single-quoted `~`, so it targeted a literal `~` directory | Expand `~` remotely (§11.5) |
| 24 | A full process snapshot on every drag-over event | Cached SSH target resolution (§10.4) |
| 25 | `wsl --list` run synchronously on the UI thread, sometimes twice per new tab | Cached, background, registry-based (§9.1) |
| 26 | Git branch failed for worktrees and submodules (`.git` file) | Follow `gitdir:` (§7.6) |
| 27 | Status bar on a 5 s poll; live-session dots on a 2 s poll | Event-driven |
| 28 | Split panes lacked env, theme, read-only, logging and the live cwd; first pane couldn't be closed while others existed; sizes reset on every rebuild | §7.4 |
| 29 | Ctrl+Shift+Arrow consumed even with one pane, which broke PSReadLine word selection | Pass it through when no move is possible |
| 30 | Tab context menu labels went stale (Pin, Broadcast) | Rebuild the menu on open |
| 31 | A tab group could not be cleared | Allow an empty value |
| 32 | CRT and minimap not persisted, and not applied to new tabs | Persisted settings |
| 33 | Workspaces never stored `sessionId`; always appended; couldn't be managed | §16.4 |
| 34 | The browser panel loaded chatgpt.com at startup even when never opened | Lazy creation (§17.1) |
| 35 | `openLink` shell-executed any URI scheme | Allowlist (§18.2) |
| 36 | Bracketed paste forced on and always applied to multi-line text, even in cmd | Honor DECSET 2004 (§5.6) |
| 37 | The image-only clipboard was never forwarded, so CLI tools couldn't receive Ctrl+V | Pass `\x16` through (§5.6) |
| 38 | `keybindings.json` never written; command palette shortcut labels hard-coded | §15 |
| 39 | Close confirmation whenever any shell was alive | Only when something is running (§7.9) |
| 40 | Every restored tab started at once | Lazy restore [P1] (§16.3) |
| 41 | Session panel search rebuilt the whole tree on each keystroke, without debounce | Debounce 100 ms, with a keyed/virtualized list |

---

## Appendix A — Shell integration scripts (verbatim behavior)

### A.1 PowerShell
This script is passed as `-NoExit -EncodedCommand <base64(UTF-16LE)>`. `{R}`, `{G}` and `{B}` are the typed-input color as decimal numbers. The final block is added only when a starting command is set; `{STARTB64}` is that command as UTF-8 base64. Omit the `__utApplyInputColor` definition when `overridePsReadLineColors` is false; the empty stub at the top then stays in effect.

```powershell
$ErrorActionPreference = 'SilentlyContinue'
$global:__utE = [string][char]27
$global:__utB = [string][char]7
function global:__utApplyInputColor { }

function global:__utWrap {
    $cur = (Get-Item Function:prompt -ErrorAction SilentlyContinue).ScriptBlock
    if ($cur -and $cur.ToString() -match '__utEmitCwdMarker') { return }
    if ($cur) { $global:__utOrigPrompt = $cur }
    function global:prompt {
        # __utEmitCwdMarker
        $__utOk  = $?
        $__utLec = $global:LASTEXITCODE
        try { __utApplyInputColor } catch {}
        try { __utWrapReadLine } catch {}

        $e = $global:__utE; $b = $global:__utB
        $out = ''

        $__utHid = -1
        try { $__utH = Get-History -Count 1 -ErrorAction SilentlyContinue; if ($__utH) { $__utHid = $__utH.Id } } catch {}
        if ($global:__utPromptSeen) {
            if ($__utHid -ne -1 -and $__utHid -eq $global:__utLastHistoryId) {
                $out += $e + ']133;D' + $b
            } else {
                $__utCode = 0
                if (-not $__utOk) {
                    if ($null -ne $__utLec -and "$__utLec" -ne '') { $__utCode = $__utLec } else { $__utCode = 1 }
                }
                $out += $e + ']133;D;' + $__utCode + $b
            }
        }
        $global:__utPromptSeen = $true
        $global:__utLastHistoryId = $__utHid

        $out += $e + ']133;A' + $b
        $p = $PWD.Path -replace '\\','/'
        $out += $e + ']7;file:///' + $p + $b

        $orig = ''
        if ($global:__utOrigPrompt) {
            try { $orig = & $global:__utOrigPrompt } catch { $orig = 'PS ' + $PWD.Path + '> ' }
        } else {
            $orig = 'PS ' + $PWD.Path + '> '
        }
        $out += [string]$orig

        $out += $e + ']133;B' + $b
        $out
    }
}

function global:__utWrapReadLine {
    $rl = Get-Item Function:PSConsoleHostReadLine -ErrorAction SilentlyContinue
    if (-not $rl) { return }
    if ($rl.ScriptBlock.ToString() -match '__utReadLineMarker') { return }
    $global:__utOrigReadLine = $rl.ScriptBlock
    function global:PSConsoleHostReadLine {
        # __utReadLineMarker
        $line = & $global:__utOrigReadLine
        try { [Console]::Write($global:__utE + ']133;C' + $global:__utB) } catch {}
        $line
    }
}

__utWrap

if (-not $global:__utOnIdleRegistered) {
    try {
        $null = Register-EngineEvent -SourceIdentifier PowerShell.OnIdle -Action {
            __utWrap
            try { __utApplyInputColor } catch {}
        }
        $global:__utOnIdleRegistered = $true
    } catch {}
}

[Console]::Write($global:__utE + ']7;file:///' + ($PWD.Path -replace '\\','/') + $global:__utB)
[Console]::Write($global:__utE + '[?2004h')

function global:__utApplyInputColor {
  if ($global:__utInputColorApplied) { return }
  try {
    Import-Module PSReadLine -ErrorAction Stop
    $vt = $global:__utE + '[38;2;{R};{G};{B}m'
    Set-PSReadLineOption -ErrorAction Stop -Colors @{
      Command = $vt; Default = $vt; Number = $vt; Parameter = $vt; Operator = $vt
      Member = $vt; Variable = $vt; Keyword = $vt; Type = $vt; String = $vt
    }
    $global:__utInputColorApplied = $true
  } catch {}
}

# Only when a starting command is set:
try {
  $__utStart = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{STARTB64}'))
  if ($__utStart) { Invoke-Expression $__utStart }
} catch {}
```

Notes on the script:
- The D marker is emitted bare when the history id hasn't changed since the last prompt. That means nothing ran: an empty Enter or a Ctrl+C at the prompt.
- **[P1] Percent-encode the OSC 7 path.** Encode at least `%`, space and `#`, using `[uri]::EscapeDataString` on each path segment. The parser decodes tolerantly.
- **[P1] Emit `ESC[?2004l` before running a command, and re-enable it at the prompt.** This is better than leaving bracketed paste on permanently.

### A.2 bash snippet (`<BASH>`)
```bash
__ut_mark=0; __ut_seen=0; __ut_pre() { case "$BASH_COMMAND" in __ut_precmd*) return;; esac; if [ "$__ut_mark" = 0 ]; then __ut_mark=1; printf '\033]133;C\007'; fi; }; __ut_precmd() { local __ut_ec=$?; if [ "$__ut_seen" = 1 ]; then printf '\033]133;D;%s\007' "$__ut_ec"; fi; __ut_seen=1; if [ -n "$__UT_OSC7" ]; then printf '\033]7;file://%s\007' "${PWD//\\//}"; fi; __ut_mark=0; return $__ut_ec; }; case "$PROMPT_COMMAND" in *__ut_precmd*) ;; *) PROMPT_COMMAND="__ut_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}";; esac; case "$PS1" in *'133;A'*) ;; *) PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]';; esac; if [ -z "$(trap -p DEBUG)" ]; then trap '__ut_pre' DEBUG; fi;
```
[P1] Under bash 5.3+, PROMPT_COMMAND can be an array. Prefer appending `__ut_precmd` to the array when it is one.

### A.3 zsh snippet (`<ZSH>`)
```zsh
__ut_seen=0; __ut_precmd() { local ec=$?; [ "$__ut_seen" = 1 ] && printf '\033]133;D;%s\007' "$ec"; __ut_seen=1; [ -n "$__UT_OSC7" ] && printf '\033]7;file://%s\007' "$PWD"; return 0; }; __ut_preexec() { printf '\033]133;C\007'; }; autoload -Uz add-zsh-hook && add-zsh-hook precmd __ut_precmd && add-zsh-hook preexec __ut_preexec; case "$PS1" in *'133;A'*) ;; *) PS1=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}';; esac;
```

### A.4 cmd PROMPT value
```
$e]133;D$e\$e]133;A$e\$e]7;file:///$P$e\$P$G$s$e]133;B$e\
```

---

## Appendix B — Theme presets (terminal colors)
Column key:

| Column | Meaning | ANSI slot |
|---|---|---|
| bg | background | — |
| fg | default text | white |
| input | the preset's typed-input suggestion. Informational only, never applied (§13.2). | — |
| muted | muted text | brightBlack |
| err | error | red |
| warn | warning | yellow |
| cmd | command / success | green |
| msg | message / info | cyan |
| acc | accent | blue |
| hi | highlight | magenta |
| cursor | cursor | — |
| selBg / selFg | selection background / foreground | — |
| N | **Y** marks presets built with `neonUi`, which uses the stronger derivation in §13.3 | — |

| # | Name | bg | fg | input | muted | err | warn | cmd | msg | acc | hi | cursor | selBg | selFg | N |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | Default | #000000 | #ffffff | #ffffff | #888888 | #ff2b7b | #ffef5c | #b4fb00 | #56ffef | #6be5ff | #c47cff | #ffffff | #ffffff | #000000 | |
| 2 | Dracula | #282a36 | #f8f8f2 | #f8f8f2 | #6272a4 | #ff5555 | #f1fa8c | #50fa7b | #8be9fd | #bd93f9 | #ff79c6 | #f8f8f2 | #44475a | #f8f8f2 | |
| 3 | Solarized Dark | #002b36 | #839496 | #93a1a1 | #586e75 | #dc322f | #b58900 | #859900 | #2aa198 | #268bd2 | #d33682 | #839496 | #073642 | #93a1a1 | |
| 4 | Monokai | #272822 | #f8f8f2 | #f8f8f2 | #75715e | #f92672 | #e6db74 | #a6e22e | #66d9ef | #ae81ff | #fd971f | #f8f8f0 | #49483e | #f8f8f2 | |
| 5 | Nord | #2e3440 | #d8dee9 | #eceff4 | #4c566a | #bf616a | #ebcb8b | #a3be8c | #88c0d0 | #81a1c1 | #b48ead | #d8dee9 | #434c5e | #eceff4 | |
| 6 | Catppuccin Mocha | #1e1e2e | #cdd6f4 | #cdd6f4 | #585b70 | #f38ba8 | #f9e2af | #a6e3a1 | #94e2d5 | #89b4fa | #cba6f7 | #f5e0dc | #45475a | #cdd6f4 | |
| 7 | One Dark | #282c34 | #abb2bf | #abb2bf | #5c6370 | #e06c75 | #e5c07b | #98c379 | #56b6c2 | #61afef | #c678dd | #abb2bf | #3e4451 | #abb2bf | |
| 8 | Gruvbox Dark | #282828 | #ebdbb2 | #ebdbb2 | #928374 | #fb4934 | #fabd2f | #b8bb26 | #8ec07c | #83a598 | #d3869b | #ebdbb2 | #3c3836 | #ebdbb2 | |
| 9 | Tokyo Night | #1a1b26 | #a9b1d6 | #c0caf5 | #565f89 | #f7768e | #e0af68 | #9ece6a | #7dcfff | #7aa2f7 | #bb9af7 | #c0caf5 | #33467c | #c0caf5 | |
| 10 | Ayu Dark | #0A0E14 | #BFBDB6 | #FFB454 | #626A73 | #F07178 | #E6B450 | #AAD94C | #95E6CB | #FFB454 | #D2A6FF | #FFB454 | #253340 | #BFBDB6 | |
| 11 | Vesper | #101010 | #FFFFFF | #FFFFFF | #666666 | #D9827A | #E8C989 | #A8C787 | #8FBCBB | #FFC799 | #C9A0DC | #FFFFFF | #2A2A2A | #FFFFFF | |
| 12 | AMOLED Green | #000000 | #f2f2f2 | #39ff14 | #6b6b6b | #ff1744 | #c6ff00 | #39ff14 | #18ffff | #00e676 | #e040fb | #f2f2f2 | #39ff14 | #000000 | Y |
| 13 | AMOLED Red | #000000 | #f2f2f2 | #ff1744 | #6b6b6b | #b71c1c | #ffab00 | #ff1744 | #ff80ab | #ff5252 | #7c4dff | #f2f2f2 | #ff1744 | #000000 | Y |
| 14 | AMOLED Purple Neon | #000000 | #f2f2f2 | #d500f9 | #6b6b6b | #ff1744 | #f50057 | #d500f9 | #00e5ff | #ea80fc | #ffea00 | #f2f2f2 | #d500f9 | #000000 | Y |
| 15 | AMOLED Cyan | #000000 | #f2f2f2 | #00e5ff | #6b6b6b | #ff1744 | #00e676 | #00e5ff | #ea80fc | #18ffff | #ff4081 | #f2f2f2 | #00e5ff | #000000 | Y |
| 16 | AMOLED Orange | #000000 | #f2f2f2 | #ff6d00 | #6b6b6b | #ff1744 | #ffea00 | #ff6d00 | #ff80ab | #ff9100 | #536dfe | #f2f2f2 | #ff6d00 | #000000 | Y |
| 17 | AMOLED Pink | #000000 | #f2f2f2 | #ff4081 | #6b6b6b | #ff1744 | #f50057 | #ff4081 | #e040fb | #ff80ab | #76ff03 | #f2f2f2 | #ff4081 | #000000 | Y |
| 18 | AMOLED Blue | #000000 | #f2f2f2 | #2979ff | #6b6b6b | #ff1744 | #00e5ff | #2979ff | #7c4dff | #448aff | #ffd600 | #f2f2f2 | #2979ff | #000000 | Y |
| 19 | AMOLED Gold | #000000 | #f2f2f2 | #ffd600 | #6b6b6b | #ff1744 | #ffab00 | #ffd600 | #ff6d00 | #ffea00 | #d500f9 | #f2f2f2 | #ffd600 | #000000 | Y |
| 20 | AMOLED Matrix | #000000 | #f2f2f2 | #00ff41 | #6b6b6b | #ff003c | #aaff00 | #00ff41 | #00e5ff | #33ff77 | #bf5fff | #f2f2f2 | #00ff41 | #000000 | Y |
| 21 | AMOLED Ice | #000000 | #f2f2f2 | #b3ffff | #6b6b6b | #ff5252 | #18ffff | #b3ffff | #ea80fc | #80d8ff | #ffd740 | #f2f2f2 | #b3ffff | #000000 | Y |
| 22 | Light | #f7f7f8 | #1a1a1a | #1565c0 | #6b7280 | #c62828 | #2e7d32 | #1565c0 | #00838f | #00838f | #6a1b9a | #1565c0 | #1565c0 | #ffffff | |
| 23 | Light Green | #f7f7f8 | #1a1a1a | #1b5e20 | #6b7280 | #c62828 | #558b2f | #1b5e20 | #2e7d32 | #2e7d32 | #00695c | #1b5e20 | #1b5e20 | #ffffff | |
| 24 | Light Red | #f7f7f8 | #1a1a1a | #b71c1c | #6b7280 | #b71c1c | #e65100 | #b71c1c | #c62828 | #c62828 | #ad1457 | #b71c1c | #b71c1c | #ffffff | |
| 25 | Light Purple Neon | #f7f7f8 | #1a1a1a | #6a1b9a | #6b7280 | #c62828 | #c2185b | #6a1b9a | #8e24aa | #8e24aa | #0277bd | #6a1b9a | #6a1b9a | #ffffff | |
| 26 | Light Cyan | #f7f7f8 | #1a1a1a | #006064 | #6b7280 | #c62828 | #00695c | #006064 | #00838f | #00838f | #4527a0 | #006064 | #006064 | #ffffff | |
| 27 | Light Orange | #f7f7f8 | #1a1a1a | #e65100 | #6b7280 | #c62828 | #f9a825 | #e65100 | #ef6c00 | #ef6c00 | #ad1457 | #e65100 | #e65100 | #ffffff | |
| 28 | Light Pink | #f7f7f8 | #1a1a1a | #ad1457 | #6b7280 | #c62828 | #d81b60 | #ad1457 | #c2185b | #c2185b | #6a1b9a | #ad1457 | #ad1457 | #ffffff | |
| 29 | Light Blue | #f7f7f8 | #1a1a1a | #0d47a1 | #6b7280 | #c62828 | #0277bd | #0d47a1 | #1565c0 | #1565c0 | #4527a0 | #0d47a1 | #0d47a1 | #ffffff | |
| 30 | Light Gold | #f7f7f8 | #1a1a1a | #f9a825 | #6b7280 | #c62828 | #ef6c00 | #f9a825 | #f57f17 | #f57f17 | #6a1b9a | #f9a825 | #f9a825 | #ffffff | |
| 31 | Light Ice | #f7f7f8 | #1a1a1a | #0277bd | #6b7280 | #c62828 | #00838f | #0277bd | #0288d1 | #0288d1 | #6a1b9a | #0277bd | #0277bd | #ffffff | |

Known issues in the Light presets, to fix [P1]:
- "Light Red" uses the same value for err and cmd (`#b71c1c`). Change cmd to `#2e7d32`.
- "Light" and "Light Cyan" share their accent, so their chrome is identical. Give "Light Cyan" a distinct accent, for example `#00796b`.
- For every Light preset, ANSI white is the dark default text (`#1a1a1a`). This is intended: "white" output stays readable on a light background. Keep it.

---

## Appendix C — Legacy (WPF) file formats, for migration
All of these files use System.Text.Json defaults: **PascalCase** keys, indented, case-sensitive.

### C.1 `settings.json`
A flat object with these keys:
- **Fonts and cursor:** `FontFamily`, `FontSize`, `FontWeight`, `CursorBlink`, `CursorStyle`, `Scrollback`.
- **Terminal colors:** `TerminalBackground`, `TextDefault`, `ColorInput`, `TextMuted`, `ColorError`, `ColorWarning`, `ColorCommand`, `ColorMessage`, `ColorAccent`, `ColorHighlight`, `CursorColor`, `SelectionBackground`, `SelectionForeground`.
- **UI:** `UiScale`, `UiFontFamily`, `UiFontSize`, `UiFontWeight`, `UiSharpness`, plus the 20 UI colors:
  `UiForeground`, `UiForegroundMuted`, `UiAccent`, `UiHighlight`, `UiSuccess`, `UiWarning`, `UiError`, `UiChromeBackground`, `UiCardBackground`, `UiCardBorder`, `UiFolderSelectedBackground`, `UiTabForeground`, `UiTabSelectedBackground`, `UiTabSelectedForeground`, `UiStatusBackground`, `UiIcon`, `UiInputBackground`, `UiInputForeground`, `UiHoverBackground`, `UiSplitter`.
- **Background and window:** `ShellBackgroundImagePath`, `ShellBackgroundImageOpacity`, `WindowBackdrop`.

**Older still (pre-semantic) keys**, which can also appear:
`Background`, `Foreground`, `White`, `Cursor`, `BrightBlack`, `Red`/`BrightRed`, `Yellow`/`BrightYellow`, `Green`/`BrightGreen`, `Cyan`/`BrightCyan`, `Blue`/`BrightBlue`, `Magenta`/`BrightMagenta`.

How the older keys map to the semantic keys:

| Semantic key | Legacy primary key | Legacy fallback key |
|---|---|---|
| TerminalBackground | Background | — |
| TextDefault | Foreground | White |
| ColorInput | Foreground | White |
| CursorColor | Cursor | — |
| TextMuted | BrightBlack | — |
| ColorError | Red | BrightRed |
| ColorWarning | Yellow | BrightYellow |
| ColorCommand | Green | BrightGreen |
| ColorMessage | Cyan | BrightCyan |
| ColorAccent | Blue | BrightBlue |
| ColorHighlight | Magenta | BrightMagenta |

### C.2 `sessions.json`
The v2 format:
```json
{ "Version": 2, "Folders": [ { "Id", "Name", "ParentId"?, "SortOrder" } ],
  "Sessions": [ { "Id", "Name", "Description", "ShellPath", "Arguments", "WorkingDirectory",
                  "StartingCommand", "ColorTag", "IconGlyph", "FolderId"?, "SortOrder",
                  "ThemeBackground", "ThemeFontSize", "EnvironmentVariables" } ] }
```
- The v1 format is a bare array of Session objects.
- `ParentId` was flattened away; folders are root-only.
- `IconGlyph` was never displayed. Ignore it.

### C.3 `snippets.json`
`[ { "Id", "Name", "Command", "SortOrder" } ]`

### C.4 `workspaces.json`
`[ { "Id", "Name", "Tabs": [ { "SessionId", "Title", "Command", "WorkingDirectory"?, "StartingCommand"? } ] } ]`

### C.5 `keybindings.json`
- Shape: `{ "Bindings": { "<action>": "<combo>" } }`
- Combos use WPF `Key` names. For example: `Ctrl+OemComma`, `Ctrl+D1`, `Ctrl+Shift+Arrow`, `Ctrl+Alt+Up`.

### C.6 `windowstate.json`
```json
{ "Left", "Top", "Width", "Height", "IsMaximized", "SessionPanelOpen", "SessionPanelWidth", "ActiveTabIndex",
  "Tabs": [ { "Title", "Command", "WorkingDirectory"?, "StartingCommand"?, "HighlightColor", "Renamed" } ] }
```
