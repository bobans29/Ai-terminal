//! PTY session.
//!
//! One child process, one master side. Unix uses a POSIX pty; Windows uses
//! ConPTY, both through `portable-pty`, so the rest of the program never
//! branches on the host. Output is delivered on a background thread via
//! `on_output` — the window loop turns that into a redraw. A future daemon
//! can own a [`PtySession`] without a window.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

pub enum PtyMsg {
    Data(Vec<u8>),
    Closed,
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    /// Input is queued here. The writer thread performs the blocking write so
    /// a child that stops reading cannot freeze the window.
    writer_tx: Option<Sender<Vec<u8>>>,
    child: Box<dyn Child + Send + Sync>,
    /// Kept so the reader thread is not detached from our point of view.
    /// It exits on its own when the master is dropped or the child dies.
    _reader: Option<JoinHandle<()>>,
    _writer: Option<JoinHandle<()>>,
}

impl PtySession {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        shell: Option<&str>,
        cwd: Option<&Path>,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
        on_output: impl Fn(PtyMsg) + Send + 'static,
    ) -> Result<Self, String> {
        if let Some(cwd) = cwd {
            let metadata = std::fs::metadata(cwd)
                .map_err(|err| format!("invalid working directory '{}': {err}", cwd.display()))?;
            if !metadata.is_dir() {
                return Err(format!(
                    "working directory '{}' is not a directory",
                    cwd.display()
                ));
            }
        }
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
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        } else if let Ok(cwd) = std::env::current_dir() {
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
        let (writer_tx, writer_rx) = mpsc::channel();
        let writer_thread = std::thread::Builder::new()
            .name("pty-writer".to_string())
            .spawn(move || write_loop(writer, writer_rx))
            .map_err(|err| format!("starting pty writer: {err}"))?;

        Ok(Self {
            master: pair.master,
            writer_tx: Some(writer_tx),
            child,
            _reader: Some(reader_thread),
            _writer: Some(writer_thread),
        })
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }
        let Some(tx) = self.writer_tx.as_ref() else {
            return Err("pty writer closed".to_string());
        };
        tx.send(data.to_vec())
            .map_err(|_| "pty writer closed".to_string())
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
        // Drop the sender so the writer thread can finish, then kill the
        // child. That unblocks a write stuck on a full PTY buffer. The
        // handles are detached on drop so shutdown does not wait on them.
        self.writer_tx.take();
        let _ = self.child.kill();
    }
}

fn write_loop(mut writer: Box<dyn Write + Send>, rx: mpsc::Receiver<Vec<u8>>) {
    while let Ok(data) = rx.recv() {
        if writer
            .write_all(&data)
            .and_then(|_| writer.flush())
            .is_err()
        {
            break;
        }
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

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{PtyMsg, PtySession};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // Exercises ConPTY directly, without requiring a window or GPU on the CI runner.
    #[test]
    fn default_shell_accepts_input_resizes_and_exits() {
        let (tx, rx) = mpsc::channel();
        let mut session = PtySession::spawn(None, None, 80, 24, 0, 0, move |message| {
            let _ = tx.send(message);
        })
        .expect("start the default Windows shell through ConPTY");
        session.resize(100, 30, 0, 0).expect("resize ConPTY");
        let size = session.master.get_size().expect("read ConPTY size");
        assert_eq!((size.cols, size.rows), (100, 30));

        // The expected marker does not occur in the input: cmd must expand the
        // environment variable, so echoed keystrokes cannot make this pass.
        session
            .write(b"echo AITERM_%TERM_PROGRAM%_READY\r")
            .expect("queue command input");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("AITERM_aiterm_READY") {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx
                .recv_timeout(remaining)
                .expect("receive shell output before timeout")
            {
                PtyMsg::Data(bytes) => output.extend(bytes),
                PtyMsg::Closed => panic!("ConPTY closed before the shell executed the command"),
            }
        }

        session.write(b"exit /b 0\r").expect("queue shell exit");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = session.child.try_wait().expect("query shell status") {
                assert!(status.success(), "shell exit status: {status:?}");
                break;
            }
            assert!(Instant::now() < deadline, "shell did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::{PtyMsg, PtySession};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("aiterm pty-日本-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&path).expect("create temporary working directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn shell_starts_in_explicit_unicode_working_directory() {
        let temp = TempDir::new();
        let expected = temp.0.to_string_lossy().into_owned();
        let (tx, rx) = mpsc::channel();
        let mut session = PtySession::spawn(
            Some("/bin/sh"),
            Some(&temp.0),
            80,
            24,
            0,
            0,
            move |message| {
                let _ = tx.send(message);
            },
        )
        .expect("start shell in explicit working directory");
        session
            .write(b"printf '\\nAITERM_CWD_%s_END\\n' \"$(pwd)\"\r")
            .expect("queue pwd");
        let marker = format!("AITERM_CWD_{expected}_END");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining).unwrap_or_else(|err| {
                panic!(
                    "receive pwd output before timeout: {err}; output: {:?}",
                    String::from_utf8_lossy(&output)
                )
            }) {
                PtyMsg::Data(bytes) => {
                    output.extend(bytes);
                    if String::from_utf8_lossy(&output).contains(&marker) {
                        break;
                    }
                }
                PtyMsg::Closed => panic!("pty closed before pwd output"),
            }
        }
        assert!(String::from_utf8_lossy(&output).contains(&marker));

        session.write(b"exit 0\n").expect("queue shell exit");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = session.child.try_wait().expect("query shell status") {
                assert!(status.success(), "shell exit status: {status:?}");
                break;
            }
            assert!(Instant::now() < deadline, "shell did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(session);
        drop(temp);
        assert!(
            !std::path::Path::new(&expected).exists(),
            "temporary directory leaked"
        );
    }

    #[test]
    fn nonexistent_working_directory_returns_path_error() {
        let path = std::env::temp_dir().join(format!("aiterm missing-日本-{}", std::process::id()));
        let error = match PtySession::spawn(Some("/bin/sh"), Some(&path), 80, 24, 0, 0, |_| {}) {
            Ok(_) => panic!("nonexistent cwd must be rejected"),
            Err(error) => error,
        };
        assert!(error.contains(&path.display().to_string()), "{error}");
    }
}
