// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2023 Robert D. French

use doors::illumos::door_h;
use doors::illumos::errno_h;
use doors::Client;
use doors::DoorArgument;
use doors::DoorCallError;
use libc;
use std::ffi::CStr;
use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
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

#[test]
fn new_door_arg() {
    let source = CString::new("Hello, World!").unwrap();
    let text = source.to_bytes_with_nul();
    let mut buffer = [0; 1024];
    let args = DoorArgument::new(text, &vec![], &mut buffer);
    let door = std::fs::File::open("/tmp/barebones_capitalize.door").unwrap();
    let door = door.as_raw_fd();

    let rc = unsafe { door_h::door_call(door, args.as_door_arg_t()) };
    if rc == -1 {
        assert_ne!(errno_h::errno(), libc::EBADF);
    }
    assert_eq!(rc, 0);
    assert_eq!(args.data().len(), 14);
    let response = std::ffi::CStr::from_bytes_with_nul(args.data()).unwrap();
    let response = response.to_str().unwrap();
    assert_eq!(response, "HELLO, WORLD!");
}

#[test]
fn dropped_doors_are_invalid() {
    let source = CString::new("Hello, World!").unwrap();
    let text = source.to_bytes_with_nul();
    let mut buffer = [0; 1024];
    let args = DoorArgument::new(text, &vec![], &mut buffer);
    let file = std::fs::File::open("/tmp/barebones_capitalize.door").unwrap();
    let fd = file.as_raw_fd();
    let door = unsafe { Client::from_raw_fd(file.into_raw_fd()) };

    let args = door.call(args).unwrap();
    assert_eq!(args.data().len(), 14);
    let response = std::ffi::CStr::from_bytes_with_nul(args.data()).unwrap();
    let response = response.to_str().unwrap();
    assert_eq!(response, "HELLO, WORLD!");

    drop(door);

    let door = unsafe { Client::from_raw_fd(fd) };
    let args = DoorArgument::new(text, &vec![], &mut buffer);

    if let Err(e) = door.call(args) {
        assert_eq!(e, DoorCallError::EBADF);
    } else {
        panic!();
    }
}

#[test]
fn open_door_from_path() {
    let source = CString::new("Hello, World!").unwrap();
    let text = source.to_bytes_with_nul();
    let mut buffer = [0; 1024];
    let args = DoorArgument::new(text, &vec![], &mut buffer);
    let door = Client::open("/tmp/barebones_capitalize.door").unwrap();

    let args = door.call(args).unwrap();
    assert_eq!(args.data().len(), 14);
    let response = std::ffi::CStr::from_bytes_with_nul(args.data()).unwrap();
    let response = response.to_str().unwrap();
    assert_eq!(response, "HELLO, WORLD!");
}

#[test]
fn call_door() {
    let source = CString::new("Hello, World!").unwrap();
    let text = source.to_bytes_with_nul();
    let mut buffer = [0; 1024];
    let args = DoorArgument::new(text, &vec![], &mut buffer);
    let file = std::fs::File::open("/tmp/barebones_capitalize.door").unwrap();
    let door = unsafe { Client::from_raw_fd(file.as_raw_fd()) };

    let args = door.call(args).unwrap();
    assert_eq!(args.data().len(), 14);
    let response = std::ffi::CStr::from_bytes_with_nul(args.data()).unwrap();
    let response = response.to_str().unwrap();
    assert_eq!(response, "HELLO, WORLD!");
}
