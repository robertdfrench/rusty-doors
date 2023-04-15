//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::illumos::door_h;
use doors::illumos::stropts_h;
use doors::server;
use doors::server::ServerProcedure;
use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::ptr;

struct Capitalize {}

impl<'a> ServerProcedure<&'a [u8]> for Capitalize {
    fn server_procedure(
        payload: server::Request<'_>,
    ) -> server::Response<&'a [u8]> {
        let original = std::str::from_utf8(payload.data).unwrap();
        let capitalized = original.to_ascii_uppercase();
        unsafe { BUFFER = capitalized };

        server::Response::new(unsafe { BUFFER.as_bytes() })
    }
}

static mut BUFFER: String = String::new();

fn main() {
    let door_path = Path::new("/tmp/fancy_capitalize.door");
    if door_path.exists() {
        fs::remove_file(door_path).unwrap();
    }
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    // Create a door for our "Capitalization Server"
    unsafe {
        // Create the (as yet unnamed) door descriptor.
        let server_door_fd =
            door_h::door_create(Capitalize::c_wrapper, ptr::null(), 0);

        // Create an empty file on the filesystem at `door_path`.
        fs::File::create(door_path).unwrap();

        // Give the door descriptor a name on the filesystem.
        stropts_h::fattach(server_door_fd, door_path_cstring.as_ptr());
    }

    std::thread::sleep(std::time::Duration::from_secs(5));
}
