# AGENTS.md

Rules for any AI agent (Codex, Cursor, Claude, etc.) working in this repo.

## Project
`aiterm`: a lightweight, fast, AI-ready terminal emulator in Rust.
Windows-first (ConPTY), also Linux/macOS. GPU rendering via `wgpu`, VT parsing via `alacritty_terminal`, PTY via `portable-pty`, config in TOML.

## Hard rules
- **Max 400 lines per file.** If a file approaches the limit or mixes concerns, split it into a submodule (`foo.rs` -> `foo/mod.rs` + focused files).
- **One responsibility per module.** Name files after what they do (`atlas.rs`, `input.rs`), not `utils.rs` / `helpers.rs`.
- **No blocking on the UI/render thread.** PTY I/O, file watching, network and AI calls run on their own threads or tasks and talk via channels.
- **AI features are opt-in and off by default.** The terminal must work fully offline with no account.
- **No `unwrap()`/`expect()` in non-test code** unless the invariant is documented in a comment. Use `anyhow` in the binary, `thiserror` in library modules.
- **No new dependency** without a one-line reason in the PR description. Prefer small crates; watch binary size and startup time.
- **Platform code stays behind `cfg`** in dedicated files (`pty/windows.rs`, `pty/unix.rs`), never scattered `#[cfg]` inside logic.

## Design principles
- **Layered, one-way dependencies:** `config` -> `pty` -> `terminal` (state) -> `renderer` -> `app` (wiring). Lower layers never import higher ones.
- **Separate state from rendering.** Terminal state is plain data; the renderer only reads it.
- **Small public surface.** Keep items `pub(crate)` or private unless needed elsewhere.
- **Data over flags.** Prefer enums and structs to boolean parameters.
- **Config is the source of truth.** Every user-facing option lives in `config.example.toml` with a sane default; hot-reload must never crash on bad config (log and keep the old one).
- **Performance budgets:** cold start < 200 ms, input-to-screen latency < 1 frame, idle CPU ~0%.
- **Security:** sanitize anything written to the PTY from paste or AI (strip ESC/control sequences); never auto-run AI-suggested commands.

## Workflow
- Work on a branch, one feature per PR, small diffs.
- Before pushing: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- Add tests for parsing, config and input encoding; keep tests next to the code (`#[cfg(test)] mod tests`) or in `tests/`.
- Update `README.md` / `config.example.toml` when behavior or options change.
- Commit messages: imperative, short (`Add tab bar rendering`).

## Roadmap (for context)
1. Core terminal (done) · 2. Tabs, splits, profiles, hot-reload · 3. Session daemon / reattach · 4. Agent-aware sidebar · 5. Search across tabs and sessions · 6. Opt-in AI layer (cloud or Ollama) · 7. Lua/WASM plugins.
Also planned: shell integration (OSC 133/7/9;9), Windows Terminal settings import, signed portable zip + winget.
