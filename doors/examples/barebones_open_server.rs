// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2023 Robert D. French

//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.
//!
//! The corresponding door client tests are located at
//! /doors/tests/barebones_open_tests.rs in this repo.
use doors::illumos::door_h;
use doors::illumos::stropts_h;
use doors::illumos::DoorFd;
use libc;
use std::ffi::{CStr, CString};
use std::fs;
use std::os::fd::IntoRawFd;
use std::path::Path;
use std::ptr;

extern "C" fn open_file(
    _cookie: *const libc::c_void,
    argp: *const libc::c_char,
    _arg_size: libc::size_t,
    _dp: *const door_h::door_desc_t,
    _n_desc: libc::c_uint,
) {
    let txt_path_cstring = unsafe { CStr::from_ptr(argp) };
    let txt_path = txt_path_cstring.to_str().unwrap();
    let file = std::fs::File::open(txt_path).unwrap();
    let dds = vec![DoorFd::new(file.into_raw_fd(), true)];
    unsafe {
        door_h::door_return(
            ptr::null(),
            0,
            dds.as_ptr() as *const door_h::door_desc_t,
            1,
        )
    };
}

fn main() {
    let door_path = Path::new("/tmp/barebones_open.door");
    if door_path.exists() {
        fs::remove_file(door_path).unwrap();
    }
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    unsafe {
        // Create the (as yet unnamed) door descriptor.
        let server_door_fd = door_h::door_create(open_file, ptr::null(), 0);

        // Create an empty file on the filesystem at `door_path`.
        fs::File::create(door_path).unwrap();

        // Give the door descriptor a name on the filesystem.
        stropts_h::fattach(server_door_fd, door_path_cstring.as_ptr());
    }

    std::thread::sleep(std::time::Duration::from_secs(5));
}
