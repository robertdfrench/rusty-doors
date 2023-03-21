/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Copyright 2021 Robert D. French
 */

//! illumos-specific APIs not (yet) found in the libc crate
//!
//! Portunus makes heavy use of illumos' [doors] facility, a novel IPC system
//! that resembles UNIX domain sockets but allows for *much* faster switching
//! between client and server contexts.  Because of its obscurity and sharp
//! corners, there is not yet a full representation of the doors API in the
//! [libc] crate.
//!
//! In this module, we represent only the subset of the illumos-specific APIs
//! that we need for Portunus.
//!
//! [doors]: https://github.com/robertdfrench/revolving-door#revolving-doors
//! [libc]: https://github.com/rust-lang/libc/tree/master/src/unix/solarish

pub mod door_h;
pub mod errno_h;
pub mod stropts_h;

#[cfg(test)]
mod tests {
    use super::*;
    use libc;
    use std::ffi::{CStr, CString};
    use std::fs;
    use std::path::Path;
    use std::ptr;

    #[test]
    fn errno_works() {
        // This test will purposefully open a nonexistant file via the libc
        // crate, and then check that errno is the expected value.
        let badpath = CString::new("<(^_^)>").unwrap();
        assert_eq!(unsafe { libc::open(badpath.as_ptr(), libc::O_RDONLY) }, -1);
        assert_eq!(errno_h::errno(), libc::ENOENT);
    }

    #[test]
    fn can_invoke_own_door() {
        let door_path = Path::new("/tmp/barebones_server.door");
        let door_path_cstring =
            CString::new(door_path.to_str().unwrap()).unwrap();

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

            // Unpack the returned bytes and compare!
            let capitalized = CStr::from_ptr(rbuf);
            let capitalized = capitalized.to_str().unwrap();
            assert_eq!(capitalized, "HELLO WORLD");

            // We did a naughty and called malloc, so we need to clean up. A PR
            // for a Rustier way to do this would be considered a personal
            // favor.
            libc::free(rbuf as *mut libc::c_void);
        }

        // Clean up the door now that we are done.
        fs::remove_file(door_path).unwrap();
    }

    #[test]
    fn retain_door_desc_t() {
        let dd = door_h::door_desc_t::new(-1, false);
        assert!(!dd.will_release());
    }

    #[test]
    fn release_door_desc_t() {
        let dd = door_h::door_desc_t::new(-1, true);
        assert!(dd.will_release());
    }
}
