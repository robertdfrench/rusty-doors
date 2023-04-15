//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::server::{Door, Request, Response};
use std::ffi::CStr;
use std::fs::File;
use std::os::fd::IntoRawFd;

#[doors::server_procedure]
fn open_file(x: Request<'_>) -> Response<[u8; 0]> {
    let txt_path_cstring = CStr::from_bytes_with_nul(x.data).unwrap();
    let txt_path = txt_path_cstring.to_str().unwrap();
    let file = File::open(txt_path).unwrap();
    Response::empty().add_descriptor(file.into_raw_fd(), true)
}

fn main() {
    let door = Door::create(open_file).unwrap();
    door.force_install("/tmp/procmac_open_server.door").unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
}
