// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2023 Robert D. French

use doors::illumos::door_h;
use libc;
use std::ffi::CStr;
use std::ffi::CString;
use std::path::Path;
use std::ptr;

/// A Barebones door client using only the illumos headers, and no additional
/// support. This helps validate that the headers are expressed correctly in
/// Rust.
///
/// The corresponding door server is located at
/// /doors/examples/barebones_open_server.rs in this repo.
#[test]
fn door_data_is_capitalized() {
    let door_path = Path::new("/tmp/barebones_capitalize.door");
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    // Send an uncapitalized string through the door and see what comes
    // back!
    let original = CString::new("hello world").unwrap();
    unsafe {
        // Connect to the Capitalization Server through its door.
        let client_door_fd =
            libc::open(door_path_cstring.as_ptr(), libc::O_RDONLY);

        // Pass `original` through the Capitalization Server's door.
        let data_ptr = original.as_ptr();
        let data_size = 12;
        let desc_ptr = ptr::null();
        let desc_num = 0;
        let rbuf = libc::malloc(data_size) as *mut libc::c_char;
        let rsize = data_size;

        let params = door_h::door_arg_t {
            data_ptr,
            data_size,
            desc_ptr,
            desc_num,
            rbuf,
            rsize,
        };

        // This is where the magic happens. We block here while control is
        // transferred to a separate thread which executes
        // `capitalize_string` on our behalf.
        door_h::door_call(client_door_fd, &params);
        libc::close(client_door_fd);

        // Unpack the returned bytes and compare!
        let capitalized = CStr::from_ptr(rbuf);
        let capitalized = capitalized.to_str().unwrap();
        assert_eq!(capitalized, "HELLO WORLD");

        // We did a naughty and called malloc, so we need to clean up. A PR
        // for a Rustier way to do this would be considered a personal
        // favor.
        libc::free(rbuf as *mut libc::c_void);
    }
}
