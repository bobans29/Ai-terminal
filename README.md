# aiterm

A cross-platform GPU terminal with a clickable tab bar. Each tab has its own terminal, PTY, title, and scrollback; background tabs keep running and producing output. Closing a tab kills its shell, and a shell that exits closes only its own tab. Closing the last tab closes the window.

New tabs use the configured shell and the process launch working directory. The renderer shares one device and glyph atlas across tabs. The project is aimed at Windows, macOS, and Linux.

## Build and run

Install Rust 1.99.1, the version used by CI. Transitive crates use Rust edition 2024, so older toolchains may fail while resolving dependencies.

```bash
rustup toolchain install 1.99.1 --profile minimal --component clippy
```

You also need a monospace font and a GPU API the process can open: Vulkan, Metal, Direct3D 12, or OpenGL ES 3. On this Linux VM the window came up on lavapipe (software Vulkan) through the same wgpu path a hardware GPU uses.

```bash
cargo +1.99.1 build --release --locked
./target/release/aiterm
```

`aiterm --help` lists the flags. Run `cargo +1.99.1 test --locked` for the automated tests and `cargo +1.99.1 clippy --locked --all-targets -- -D warnings` for lint checks. CI runs both and a release build natively on Linux and Windows; a successful CI build does not establish that the desktop works.

The first launch prints the config path it looked for and the GPU adapter it opened.

### Windows build

Use Windows 10 version 1809 or later (ConPTY is required), or Windows 11, on x86-64. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022) with **Desktop development with C++**, including the MSVC x64 build tools and a Windows SDK, then install Rust with the MSVC host toolchain using [rustup](https://rustup.rs/). A Direct3D 12 or Vulkan-capable graphics driver and a monospace font such as Consolas are required.

From a PowerShell terminal in the checkout:

```powershell
rustup toolchain install 1.99.1-x86_64-pc-windows-msvc --profile minimal --component clippy
cargo +1.99.1-x86_64-pc-windows-msvc build --release --locked
.\target\release\aiterm.exe
```

If the linker cannot find the Windows SDK or MSVC libraries, use the **Developer PowerShell for Visual Studio** installed with the build tools. The default shell is `%COMSPEC%` (normally `cmd.exe`). To use Windows PowerShell, set `shell = "powershell.exe"`; for PowerShell 7 use `shell = "pwsh.exe"` after installing it. These are executable paths, not command lines. Use a TOML literal string for a full Windows path, for example `shell = 'C:\Program Files\PowerShell\7\pwsh.exe'`.

After a successful Windows CI job, download **aiterm-windows-x86_64** from that workflow run's **Artifacts** section. The archive contains `aiterm.exe`, the example config, and this README. It is an unsigned executable, not an installer. Extract it before running it; keep your personal configuration at `%APPDATA%\aiterm\config.toml` or pass `--config` explicitly. No successful Windows CI result or desktop validation is implied by adding the workflow.

### Windows desktop smoke check

Run this checklist on a Windows desktop before treating Windows support as verified. These interactive checks have not been run in the Linux cloud environment:

1. Launch with no config and confirm a window, readable Consolas fallback, a GPU adapter in the startup log, and a working `cmd.exe` prompt.
2. Run `echo aiterm` and `chcp 65001`, then type and print non-ASCII text. Repeat using a config with Windows PowerShell and, when installed, PowerShell 7. Test an executable path containing spaces and a working directory containing spaces and non-ASCII characters.
3. Run a long command such as `ping -t 127.0.0.1`, interrupt it with Ctrl+C, and confirm the shell stays usable. Resize the window and test a full-screen terminal application.
4. Select text, copy with Ctrl+Shift+C, paste with Ctrl+Shift+V and Shift+Insert, and check multiline and Unicode clipboard text.
5. Move the window between displays with different scaling, or change Windows display scaling. Check glyphs, cursor placement, mouse selection, and terminal dimensions.
6. Run `exit` and confirm the window closes. Launch again, close the window while its shell is running, and check that its shell process terminates. Record the Windows version, GPU adapter, shell, and outcomes when reporting issues.

## Architecture

One binary, `aiterm`. The modules are the seams later milestones hang off; they are not separate crates yet.

| Module | Role |
| --- | --- |
| `app` | winit window loop and tab bar. |
| `terminal` | One VT grid per tab. |
| `pty` | Child process and PTY. A headless session can own this without a window. |
| `renderer` | Glyph atlas and wgpu frame, shared across tabs. |
| `input` | xterm key and mouse sequences. |
| `config`, `color`, `font` | TOML, palette, monospace face lookup. |

**Terminal core.** VT parsing is [alacritty_terminal](https://github.com/alacritty/alacritty) 0.24 (the `vte` crate underneath it), not a parser written for this project. That grid already implements the sequences common programs need: SGR colors including 256-color and truecolor, cursor movement, erase, scrollback, and the alternate screen so vim, htop, and less work. Reimplementing that is a long tail of edge cases for no gain in Milestone 1.

**PTY.** [portable-pty](https://github.com/wezterm/wezterm) opens a POSIX pty on Unix and ConPTY on Windows behind one API. With no `shell` key, Unix launches the login shell (`$SHELL`, then passwd) and Windows launches `%COMSPEC%` (usually `cmd.exe`). Set `shell = "powershell.exe"` for PowerShell. `TERM` is `xterm-256color` and `COLORTERM` is `truecolor`.

**Rendering.** Alacritty's own renderer is OpenGL and is tied to its window. aiterm keeps the grid and draws it with wgpu: fontdue rasterizes one codepoint into an R8 atlas, and the frame is solid cells plus glyph quads. Tabs share the renderer's device and atlas. It uses Vulkan, Metal, or D3D12 on a real GPU.

## Config

TOML. A missing file is not an error; the window opens with the defaults. A file that exists but does not parse is an error.

Search order:

1. `--config <path>` / `-c <path>`
2. `$AITERM_CONFIG`
3. Platform path:
   - Linux: `$XDG_CONFIG_HOME/aiterm/config.toml` or `~/.config/aiterm/config.toml`
   - macOS: `~/Library/Application Support/aiterm/config.toml`
   - Windows: `%APPDATA%\aiterm\config.toml`

`config.example.toml` is the full file. Every key is optional.

```toml
font_family = "JetBrains Mono"
font_size = 15.0
scrollback = 10000
# Program path, not a shell command line. Unset = login shell / %COMSPEC%.
# shell = "powershell.exe"

[colors]
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor = "#f5e0dc"
selection = "#45475a"
# black, red, green, yellow, blue, magenta, cyan, white
# bright_black … bright_white
```

`font_size` is pixels at scale 1 and follows the window scale factor. It is clamped to 6–72. Scrollback is capped at 200,000 lines. 256-color and truecolor values are computed, not configured. Unknown keys are rejected.

## Keys and mouse

Ctrl combinations, arrows (including application cursor mode), Home/End, Page Up/Down, and F1–F12 are encoded as xterm sequences. The window resize resizes the PTY.

The mouse wheel scrolls the scrollback (three lines per notch). Shift+Page Up / Shift+Page Down do the same. Drag with the left button to select; releasing copies to the clipboard. Paste is Ctrl+Shift+V, copy is Ctrl+Shift+C. On macOS those are Command+C and Command+V. Shift+Insert also pastes. On Linux the selection is also written to the primary clipboard. The clipboard handle stays open for the life of the window so the selection remains available on X11 without a clipboard manager.

Click a tab to select it. Click **+** or press Ctrl+Shift+T to open a tab; press Ctrl+Shift+W to close the active tab. On macOS, Cmd+T and Cmd+W open and close tabs. Ctrl+Tab and Ctrl+Shift+Tab cycle through tabs.

## Known limitations

- No splits, profiles, search, hyperlinks, or bell.
- The config is read once at startup. There is no hot reload.
- Glyphs are one codepoint per cell. No ligatures and no text shaping. Combining marks are not drawn on top of the base character. Wide characters use the core's wide-cell flags.
- Bold and italic are used when a distinct font file exists. Otherwise bold is a one-pixel overstrike and italic is the regular face.
- IME commit strings are written to the shell. The preedit popup is not drawn. This path has not been tried on Windows.
- Mouse reporting is SGR and a basic X10 encoding. There is no kitty keyboard protocol.
- In the alternate screen, the wheel sends arrow keys (vim/less) instead of scrolling history. Shift+wheel still scrolls. Touchpad pixel deltas accumulate until they cover a full cell.
- Bracketed paste strips ESC bytes, so clipboard text cannot end paste mode early and run as shell input.
- `shell` is a program path. It is not parsed as a command line.
- Windows build, tests, and Clippy have a native CI workflow, but its results must be checked on the submitted revision. Interactive Windows behavior still needs the desktop smoke check above. macOS has not been built or run. Linux validation used lavapipe, a software Vulkan adapter, so it does not prove hardware GPU operation.

## Next steps

Splits, profiles, hot reload, search, and agent features are future work.
