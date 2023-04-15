//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::illumos::door_h;
use doors::illumos::stropts_h;
use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::ptr;

#[doors::server_procedure]
fn double(payload: u8) -> u8 {
    payload * 2
}

fn main() {
    let door_path = Path::new("/tmp/procmac_double.door");
    if door_path.exists() {
        fs::remove_file(door_path).unwrap();
    }
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    // Create a door for our "Capitalization Server"
    unsafe {
        // Create the (as yet unnamed) door descriptor.
        let server_door_fd = door_h::door_create(double, ptr::null(), 0);

        // Create an empty file on the filesystem at `door_path`.
        fs::File::create(door_path).unwrap();

        // Give the door descriptor a name on the filesystem.
        stropts_h::fattach(server_door_fd, door_path_cstring.as_ptr());
    }

    std::thread::sleep(std::time::Duration::from_secs(5));
}
