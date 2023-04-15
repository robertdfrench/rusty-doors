//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::illumos::door_h;
use doors::illumos::stropts_h;
use libc;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::Path;
use std::ptr;

// The simplest possible smoke test is to see if we can both call and
// answer our own door invocation. Remember: door_create does not change
// control, but door_call and door_return do. So we only need one thread
// to pull this off.
extern "C" fn capitalize_string(
    _cookie: *const libc::c_void,
    argp: *const libc::c_char,
    arg_size: libc::size_t,
    _dp: *const door_h::door_desc_t,
    _n_desc: libc::c_uint,
) {
    // Capitalize the string provided by the client. This is a lazy way
    // to verify that we are able to send and receive data through
    // doors. We aren't testing descriptors, because we aren't really
    // testing doors itself, just making sure our Rust interface works.
    let original = unsafe { CStr::from_ptr(argp) };
    let original = original.to_str().unwrap();
    let capitalized = original.to_ascii_uppercase();
    let capitalized = CString::new(capitalized).unwrap();
    unsafe {
        door_h::door_return(capitalized.as_ptr(), arg_size, ptr::null(), 0)
    };
}

fn main() {
    let door_path = Path::new("/tmp/barebones_server.door");
    if door_path.exists() {
        fs::remove_file(door_path).unwrap();
    }
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    // Create a door for our "Capitalization Server"
    unsafe {
        // Create the (as yet unnamed) door descriptor.
        let server_door_fd =
            door_h::door_create(capitalize_string, ptr::null(), 0);

        // Create an empty file on the filesystem at `door_path`.
        fs::File::create(door_path).unwrap();

        // Give the door descriptor a name on the filesystem.
        stropts_h::fattach(server_door_fd, door_path_cstring.as_ptr());
    }

    std::thread::sleep(std::time::Duration::from_secs(5));
}
