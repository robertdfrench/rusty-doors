//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::server;
use doors::server::ServerProcedure;
use std::fs;
use std::os::fd::IntoRawFd;
use std::path::Path;

struct OpenFile {}

impl ServerProcedure for OpenFile {
    fn server_procedure(payload: server::Request<'_>) -> server::Response {
        let txt_path = std::str::from_utf8(payload.data).unwrap();
        let file = std::fs::File::open(txt_path).unwrap();

        server::Response::empty().add_descriptor(file.into_raw_fd(), true)
    }
}

fn main() {
    let door_path = Path::new("/tmp/fancy_open_server.door");
    if door_path.exists() {
        fs::remove_file(door_path).unwrap();
    }
    OpenFile::install(0, "/tmp/fancy_open_server.door", 0).unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
}
