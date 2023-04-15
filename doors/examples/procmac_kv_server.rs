//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::illumos::door_h;
use doors::illumos::stropts_h;
use doors::server;
use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicU8, Ordering};

static mut COUNT: AtomicU8 = AtomicU8::new(0);

#[doors::server_procedure]
fn increment(_payload: server::Request<'_>) -> server::Response<[u8; 0]> {
    unsafe { COUNT.fetch_add(1, Ordering::SeqCst) };
    server::Response::empty()
}

#[doors::server_procedure]
fn fetch(_payload: server::Request<'_>) -> server::Response<[u8; 1]> {
    let x = unsafe { COUNT.load(Ordering::SeqCst) };
    server::Response::new([x])
}

fn main() {
    let door_path = Path::new("/tmp/procmac_kv_store.door");
    if door_path.exists() {
        fs::remove_file(door_path).unwrap();
    }
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    // Create a door for our "Capitalization Server"
    unsafe {
        // Create the (as yet unnamed) door descriptor.
        let server_door_fd = door_h::door_create(increment, ptr::null(), 0);

        // Create an empty file on the filesystem at `door_path`.
        fs::File::create(door_path).unwrap();

        // Give the door descriptor a name on the filesystem.
        stropts_h::fattach(server_door_fd, door_path_cstring.as_ptr());
    }

    let door_path = Path::new("/tmp/procmac_kv_fetch.door");
    if door_path.exists() {
        fs::remove_file(door_path).unwrap();
    }
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    // Create a door for our "Capitalization Server"
    unsafe {
        // Create the (as yet unnamed) door descriptor.
        let server_door_fd = door_h::door_create(fetch, ptr::null(), 0);

        // Create an empty file on the filesystem at `door_path`.
        fs::File::create(door_path).unwrap();

        // Give the door descriptor a name on the filesystem.
        stropts_h::fattach(server_door_fd, door_path_cstring.as_ptr());
    }

    std::thread::sleep(std::time::Duration::from_secs(5));
}
