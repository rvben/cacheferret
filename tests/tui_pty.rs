#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const BIN: &str = env!("CARGO_BIN_EXE_cacheferret");

#[test]
fn tui_enters_draws_and_restores_the_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("demo");
    fs::create_dir_all(project.join("target")).unwrap();
    fs::write(project.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(BIN);
    command.args([
        "tui",
        "--root",
        temp.path().to_str().unwrap(),
        "--scope",
        "project",
    ]);
    command.env("TERM", "xterm-256color");
    command.env("LANG", "C.UTF-8");
    command.env("CACHEFERRET_REDUCE_MOTION", "1");
    command.env("CACHEFERRET_NO_CACHE", "1");

    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let reader_thread = thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        output
    });
    let mut writer = pair.master.take_writer().unwrap();

    thread::sleep(Duration::from_millis(150));
    writer.write_all(b"q").unwrap();
    writer.flush().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("TUI did not exit after q");
        }
        thread::sleep(Duration::from_millis(20));
    };
    drop(writer);
    drop(pair.master);
    let output = String::from_utf8_lossy(&reader_thread.join().unwrap()).into_owned();

    assert!(status.success(), "status: {status:?}\n{output}");
    assert!(
        output.contains("\u{1b}[?1049h"),
        "alternate screen not entered"
    );
    assert!(output.contains("\u{1b}[?25l"), "cursor not hidden");
    assert!(output.contains("CACHE"), "TUI was not drawn");
    assert!(output.contains("FERRET"), "TUI was not drawn");
    assert!(
        output.contains("\u{1b}[?1049l"),
        "alternate screen not left"
    );
    assert!(output.contains("\u{1b}[?25h"), "cursor not restored");
}
