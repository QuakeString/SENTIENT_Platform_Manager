// Copyright © 2016-2026 The SENTIENT Authors

//! One terminal session: a program in a pseudo terminal of this computer
//! (portable-pty: Unix PTYs, Windows ConPTY), its output parsed into a
//! screen grid with scrollback (alacritty_terminal), and what is typed sent
//! back. On a server the program is `ssh -tt` (crate::remote), so the
//! server gets a real terminal and its size; a resize here is a resize
//! there.
//!
//! Three threads per session: the reader (output into the grid, then a
//! wake-up for the view), the writer (keys, pastes and the grid's own
//! answers such as the cursor position), the waiter (the program's end;
//! on Windows closing the ConPTY is what ends the reader). No lock is held
//! across anything that waits on another thread's lock.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, Timeout};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize};

/// Lines kept above the screen.
const SCROLLBACK: usize = 10_000;

/// What to start: the program and its arguments, extra variables, the
/// folder to start in.
#[derive(Clone, Debug)]
pub struct Spawn {
    pub argv: Vec<String>,
    pub env: Vec<(String, OsString)>,
    pub cwd: Option<std::path::PathBuf>,
}

/// The grid's size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }

    fn screen_lines(&self) -> usize {
        self.rows as usize
    }

    fn columns(&self) -> usize {
        self.cols as usize
    }
}

/// Synchronized updates (BSU/ESU) are drawn as they arrive: there is no
/// timer here to end one a program never ends.
#[derive(Default)]
pub struct NoSync;

impl Timeout for NoSync {
    fn set_timeout(&mut self, _: std::time::Duration) {}
    fn clear_timeout(&mut self) {}
    fn pending_timeout(&self) -> bool {
        false
    }
}

/// The grid's requests: answers to the program (cursor position, device
/// attributes) and the shell's name (the title a Linux session sets first,
/// crate::terminal::prompt::linux_script).
#[derive(Clone)]
pub struct Listener {
    to_pty: mpsc::Sender<Vec<u8>>,
    shell: Arc<Mutex<Option<String>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                let _ = self.to_pty.send(text.into_bytes());
            }
            Event::Title(t) => {
                if let Some(name) = super::prompt::shell_from_title(&t) {
                    if let Ok(mut g) = self.shell.lock() {
                        g.get_or_insert(name);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The parser and the grid, locked together.
pub struct Emu {
    pub term: Term<Listener>,
    parser: Processor<NoSync>,
}

impl Emu {
    /// Feed bytes as if the program had written them (the session's own
    /// notes, dim).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }
}

pub struct Session {
    pub emu: Arc<Mutex<Emu>>,
    to_pty: mpsc::Sender<Vec<u8>>,
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    alive: Arc<AtomicBool>,
    pub shell: Arc<Mutex<Option<String>>>,
    size: Mutex<GridSize>,
}

impl Session {
    /// Start `spawn` in a new pseudo terminal of `size`. `wake` is called
    /// (from a session thread) whenever there is something new to draw.
    pub fn start(spawn: &Spawn, size: GridSize, wake: impl Fn() + Send + Sync + 'static) -> Result<Session, String> {
        let pty = portable_pty::native_pty_system();
        let pair = pty.openpty(PtySize { rows: size.rows, cols: size.cols, pixel_width: 0, pixel_height: 0 }).map_err(|e| nf!("The terminal could not be opened: {e}"))?;
        let mut cmd = CommandBuilder::from_argv(spawn.argv.iter().map(OsString::from).collect());
        for (k, v) in &spawn.env {
            cmd.env(k, v);
        }
        if let Some(d) = &spawn.cwd {
            cmd.cwd(d);
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(|e| nf!("{} could not be started: {e}", spawn.argv.first().map(String::as_str).unwrap_or(n!("The shell"))))?;
        // Only the program keeps the terminal's other end: its end is then
        // the reader's end (Unix).
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let killer = child.clone_killer();

        let (to_pty, from_ui) = mpsc::channel::<Vec<u8>>();
        let shell = Arc::new(Mutex::new(None));
        let listener = Listener { to_pty: to_pty.clone(), shell: shell.clone() };
        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        let emu = Arc::new(Mutex::new(Emu { term: Term::new(config, &size, listener), parser: Processor::<NoSync>::new() }));
        let alive = Arc::new(AtomicBool::new(true));
        let master = Arc::new(Mutex::new(Some(pair.master)));
        let wake = Arc::new(wake);

        // Writer: everything for the program, in order.
        std::thread::Builder::new()
            .name("spm-term-write".into())
            .spawn(move || {
                while let Ok(bytes) = from_ui.recv() {
                    if writer.write_all(&bytes).and_then(|_| writer.flush()).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        // Reader: output into the grid (the lock only while parsing).
        {
            let (emu, alive, wake) = (emu.clone(), alive.clone(), wake.clone());
            std::thread::Builder::new()
                .name("spm-term-read".into())
                .spawn(move || {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Ok(mut e) = emu.lock() {
                                    e.feed(&buf[..n]);
                                }
                                wake();
                            }
                        }
                    }
                    alive.store(false, Ordering::SeqCst);
                    wake();
                })
                .map_err(|e| e.to_string())?;
        }

        // Waiter: the program ended; close the terminal so the reader ends
        // too (a ConPTY keeps its output open until it is closed).
        {
            let (master, alive, wake) = (master.clone(), alive.clone(), wake.clone());
            std::thread::Builder::new()
                .name("spm-term-wait".into())
                .spawn(move || {
                    let _ = child.wait();
                    alive.store(false, Ordering::SeqCst);
                    let closing = master.lock().ok().and_then(|mut g| g.take());
                    drop(closing);
                    wake();
                })
                .map_err(|e| e.to_string())?;
        }

        Ok(Session { emu, to_pty, master, killer: Mutex::new(killer), alive, shell, size: Mutex::new(size) })
    }

    /// The program still runs (the connection is up, for ssh).
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Send bytes to the program (keys, a paste). Scrolled back, the view
    /// returns to the bottom, as terminals do.
    pub fn write(&self, bytes: Vec<u8>) {
        if bytes.is_empty() || !self.alive() {
            return;
        }
        if let Ok(mut e) = self.emu.lock() {
            if e.term.grid().display_offset() != 0 {
                e.term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
            }
        }
        let _ = self.to_pty.send(bytes);
    }

    pub fn size(&self) -> GridSize {
        self.size.lock().map(|g| *g).unwrap_or(GridSize { cols: 80, rows: 24 })
    }

    /// A new size: the grid reflows and the program is told (SIGWINCH;
    /// over ssh a window-change to the server).
    pub fn resize(&self, size: GridSize) {
        if size.cols == 0 || size.rows == 0 {
            return;
        }
        {
            let Ok(mut s) = self.size.lock() else { return };
            if *s == size {
                return;
            }
            *s = size;
        }
        if let Ok(mut e) = self.emu.lock() {
            e.term.resize(size);
        }
        if let Ok(g) = self.master.lock() {
            if let Some(m) = g.as_ref() {
                let _ = m.resize(PtySize { rows: size.rows, cols: size.cols, pixel_width: 0, pixel_height: 0 });
            }
        }
    }

    /// End the session: the program is stopped (ssh's end closes the
    /// server's shell) and the terminal closed.
    pub fn end(&self) {
        if let Ok(mut k) = self.killer.lock() {
            let _ = k.kill();
        }
        let closing = self.master.lock().ok().and_then(|mut g| g.take());
        drop(closing);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real session: a shell in a pseudo terminal, its output in the
    /// grid, its size, its end.
    #[cfg(unix)]
    #[test]
    fn a_local_program_draws_into_the_grid_and_ends() {
        let spawn = Spawn { argv: vec!["/bin/sh".into(), "-c".into(), "printf 'hello\\033[31m red\\033[0m'; stty size; sleep 0.3".into()], env: vec![("TERM".into(), "xterm-256color".into())], cwd: None };
        let (tx, rx) = mpsc::channel::<()>();
        let tx = Mutex::new(tx);
        let s = Session::start(&spawn, GridSize { cols: 100, rows: 30 }, move || {
            let _ = tx.lock().map(|t| t.send(()));
        })
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while s.alive() && std::time::Instant::now() < deadline {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
        }
        assert!(!s.alive());
        let e = s.emu.lock().unwrap();
        let text: String = (0..2)
            .map(|l| {
                let row = &e.term.grid()[alacritty_terminal::index::Line(l)];
                (0..100).map(|c| row[alacritty_terminal::index::Column(c)].c).collect::<String>().trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.starts_with("hello red"), "{text}");
        assert!(text.contains("30 100"), "stty saw the size: {text}");
    }
}
