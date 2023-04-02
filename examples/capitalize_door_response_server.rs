//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::illumos::door_h;
use doors::illumos::door_h::door_desc_t;
use doors::illumos::stropts_h;
use libc;
use std::ffi::CString;
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
    dp: *const door_h::door_desc_t,
    n_desc: libc::c_uint,
) {
    let (r_data, r_desc) = inner(
        unsafe { std::slice::from_raw_parts(argp as *const u8, arg_size) },
        unsafe { std::slice::from_raw_parts(dp, n_desc.try_into().unwrap()) },
    );

    unsafe {
        door_h::door_return(
            r_data.as_ptr() as *const i8,
            r_data.len(),
            r_desc.as_ptr(),
            r_desc.len().try_into().unwrap(),
        );
    }
}

static mut BUFFER: String = String::new();

fn inner<'a, 'b>(
    data: &'a [u8],
    _desc: &'a [door_desc_t],
) -> (&'b [u8], &'b [door_desc_t]) {
    let original = std::str::from_utf8(data).unwrap();
    let capitalized = original.to_ascii_uppercase();
    unsafe { BUFFER = capitalized };

    (unsafe { BUFFER.as_bytes() }, &[])
}

fn main() {
    let door_path = Path::new("/tmp/capitalize_door_response.door");
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
