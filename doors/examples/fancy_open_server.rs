//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::server;
use doors::server::ServerProcedure;
use std::os::fd::IntoRawFd;

struct OpenFile {}

impl<'a> ServerProcedure<&'a [u8]> for OpenFile {
    fn server_procedure(
        payload: server::Request<'_>,
    ) -> server::Response<&'a [u8]> {
        let txt_path = std::str::from_utf8(payload.data).unwrap();
        let file = std::fs::File::open(txt_path).unwrap();

        server::Response::empty().add_descriptor(file.into_raw_fd(), true)
    }
}

fn main() {
    let open_file = OpenFile::create_server().unwrap();
    std::fs::remove_file("/tmp/fancy_open_server.door").unwrap();
    open_file.install("/tmp/fancy_open_server.door").unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
}
