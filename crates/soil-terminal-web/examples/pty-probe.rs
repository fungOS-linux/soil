//! Probe: does a pty deliver a child's output on this machine?

use std::io::Read;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn main() {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 6,
            cols: 40,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut builder = CommandBuilder::new("/bin/echo");
    builder.arg("hello from the pty");
    let _child = pair.slave.spawn_command(builder).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut buffer = [0u8; 1024];
    match reader.read(&mut buffer) {
        Ok(0) => println!("eof"),
        Ok(n) => println!("read {n}: {:?}", String::from_utf8_lossy(&buffer[..n])),
        Err(error) => println!("error: {error}"),
    }
}
