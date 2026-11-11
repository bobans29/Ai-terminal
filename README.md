# aiterm

A cross-platform GPU terminal. Milestone 1 is one window, one shell, usable for ordinary work: colors, full-screen programs, scrollback, and copy/paste.

Later milestones (not in this tree) are aimed at a lightweight, customizable, agent-ready terminal: tabs with live agent status, an agent sidebar, headless sessions, and global search, on Windows, macOS, and Linux.

## Build and run

Install a current stable Rust toolchain (this tree was built with Rust 1.99). Transitive crates use Rust edition 2024, so an older toolchain will fail while resolving dependencies.

You also need a monospace font and a GPU API the process can open: Vulkan, Metal, Direct3D 12, or OpenGL ES 3. On this Linux VM the window came up on lavapipe (software Vulkan) through the same wgpu path a hardware GPU uses.

```bash
cargo build --release
./target/release/aiterm
```

`aiterm --help` lists the flags. `cargo test` covers the palette, the config parser, a few VT sequences, and key encoding. `cargo clippy --all-targets -- -D warnings` is clean.

The first launch prints the config path it looked for and the GPU adapter it opened.

## Architecture

One binary, `aiterm`. The modules are the seams later milestones hang off; they are not separate crates yet.

| Module | Role |
| --- | --- |
| `app` | winit window loop. Tabs, splits, and an agent sidebar belong here. |
| `terminal` | One VT grid. A future pane is another instance. |
| `pty` | Child process and PTY. A headless session can own this without a window. |
| `renderer` | Glyph atlas and wgpu frame. Tabs would share the device and the atlas. |
| `input` | xterm key and mouse sequences. |
| `config`, `color`, `font` | TOML, palette, monospace face lookup. |

**Terminal core.** VT parsing is [alacritty_terminal](https://github.com/alacritty/alacritty) 0.24 (the `vte` crate underneath it), not a parser written for this project. That grid already implements the sequences common programs need: SGR colors including 256-color and truecolor, cursor movement, erase, scrollback, and the alternate screen so vim, htop, and less work. Reimplementing that is a long tail of edge cases for no gain in Milestone 1.

**PTY.** [portable-pty](https://github.com/wezterm/wezterm) opens a POSIX pty on Unix and ConPTY on Windows behind one API. With no `shell` key, Unix launches the login shell (`$SHELL`, then passwd) and Windows launches `%COMSPEC%` (usually `cmd.exe`). Set `shell = "powershell.exe"` for PowerShell. `TERM` is `xterm-256color` and `COLORTERM` is `truecolor`.

**Rendering.** Alacritty's own renderer is OpenGL and is tied to its window. aiterm keeps the grid and draws it with wgpu: fontdue rasterizes one codepoint into an R8 atlas, and the frame is solid cells plus glyph quads. That leaves room for a sidebar and tab strip in the same pass later, and it uses Vulkan, Metal, or D3D12 on a real GPU.

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

Ctrl+Tab is swallowed so a later tab switcher can have it.

## Known limitations

- One window, one shell. No tabs, splits, profiles, search, hyperlinks, or bell.
- The config is read once at startup. There is no hot reload.
- Glyphs are one codepoint per cell. No ligatures and no text shaping. Combining marks are not drawn on top of the base character. Wide characters use the core's wide-cell flags.
- Bold and italic are used when a distinct font file exists. Otherwise bold is a one-pixel overstrike and italic is the regular face.
- IME commit strings are written to the shell. The preedit popup is not drawn. This path has not been tried on Windows.
- Mouse reporting is SGR and a basic X10 encoding. There is no kitty keyboard protocol.
- In the alternate screen, the wheel sends arrow keys (vim/less) instead of scrolling history. Shift+wheel still scrolls.
- `shell` is a program path. It is not parsed as a command line.
- Windows and macOS code paths are `cfg`-gated and compile as part of the same sources, but they have not been built or run on those systems. The owner is on Windows: ConPTY and the default `%COMSPEC%` shell are the pieces most worth trying first. The Linux demo below used lavapipe, a software Vulkan adapter, so it does not prove a hardware GPU.

## Milestone 2

Suggested next steps, in an order that keeps the current modules:

1. **Tabs.** A tab owns a `Terminal` plus a `PtySession`. `app` draws a tab strip and routes input to the active pair. The renderer keeps one device and one atlas.
2. **Splits.** Same pair, with a layout tree instead of a single grid origin.
3. **Profiles.** Named config blocks (`shell`, font, palette, cwd) that a new tab can pick up.
4. **Hot reload.** Watch the TOML file and rebuild the palette and, when the font changes, the atlas, without restarting the shell.
