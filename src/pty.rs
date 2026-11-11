//! PTY session.
//!
//! One child process, one master side. Unix uses a POSIX pty; Windows uses
//! ConPTY, both through `portable-pty`, so the rest of the program never
//! branches on the host. Output is delivered on a background thread via
//! `on_output` — the window loop turns that into a redraw. A future daemon
//! can own a [`PtySession`] without a window.

use std::io::{Read, Write};
use std::thread::JoinHandle;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub enum PtyMsg {
    Data(Vec<u8>),
    Closed,
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    /// Kept so the reader thread is not detached from our point of view.
    /// It exits on its own when the master is dropped or the child dies.
    _reader: Option<JoinHandle<()>>,
}

impl PtySession {
    pub fn spawn(
        shell: Option<&str>,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
        on_output: impl Fn(PtyMsg) + Send + 'static,
    ) -> Result<Self, String> {
        let size = PtySize {
            rows,
            cols,
            pixel_width,
            pixel_height,
        };
        let system = native_pty_system();
        let pair = system
            .openpty(size)
            .map_err(|err| format!("opening pty: {err}"))?;

        let mut cmd = match shell {
            Some(program) => CommandBuilder::new(program),
            None => CommandBuilder::new_default_prog(),
        };
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "aiterm");
        if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }

        let shell_name = shell.map(str::to_string).unwrap_or_else(|| cmd.get_shell());
        eprintln!("aiterm: shell {shell_name} ({cols}x{rows})");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| format!("spawning shell: {err}"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| format!("cloning pty reader: {err}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| format!("taking pty writer: {err}"))?;

        let reader_thread = std::thread::Builder::new()
            .name("pty-reader".to_string())
            .spawn(move || read_loop(reader, on_output))
            .map_err(|err| format!("starting pty reader: {err}"))?;

        Ok(Self {
            master: pair.master,
            writer,
            child,
            _reader: Some(reader_thread),
        })
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer
            .write_all(data)
            .map_err(|err| format!("writing to pty: {err}"))?;
        self.writer
            .flush()
            .map_err(|err| format!("flushing pty: {err}"))
    }

    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), String> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width,
                pixel_height,
            })
            .map_err(|err| format!("resizing pty: {err}"))
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn read_loop(mut reader: Box<dyn Read + Send>, on_output: impl Fn(PtyMsg)) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                on_output(PtyMsg::Closed);
                break;
            }
            Ok(n) => on_output(PtyMsg::Data(buf[..n].to_vec())),
            Err(_) => {
                on_output(PtyMsg::Closed);
                break;
            }
        }
    }
}
