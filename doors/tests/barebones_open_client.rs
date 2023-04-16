// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2023 Robert D. French

use doors::illumos::door_h;
use libc;
use std::ffi::CString;
use std::io::Read;
use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;
use std::path::Path;
use std::ptr;

/// A Barebones door client using only the illumos headers, and no additional
/// support. This helps validate that the headers are expressed correctly in
/// Rust.
///
/// The corresponding door server is located at
/// /doors/examples/barebones_open_server.rs in this repo.
#[test]
fn can_read_from_returned_descriptor() {
    // We need to prepare a C String to pass to libc::open
    let door_path = Path::new("/tmp/barebones_open.door");
    let door_path_cstring = CString::new(door_path.to_str().unwrap()).unwrap();

    // This is the file that we will ask the open server to open for us. Before
    // we do that, we stage some text in it. That way when we get the file
    // descriptor back, we can *read* from it to see if we get the expected text
    // back.
    let txt_path_str = "/tmp/barebones_open.txt";
    let txt_path = Path::new(&txt_path_str);
    let mut txt = std::fs::File::create(txt_path).expect("create txt");
    writeln!(txt, "Hello, World!").expect("write txt");

    // We drop this file so that we can be sure its contents are flushed to
    // disk, and also to protect the integrity of this test -- we want to
    // guarantee that we are reading its contents from disk as though it were a
    // newly opened file.
    drop(txt);

    // Connect to the Open Server through its door. We just use plain-old libc
    // open here, since we can pass the resulting descriptor to door_call.
    let client_door_fd =
        unsafe { libc::open(door_path_cstring.as_ptr(), libc::O_RDONLY) };

    // We prepare the parameters for our door invocation. In this case, we are
    // sending the path to the /tmp/barebones_open_server.txt file as the 'data'
    // field, and we expect to receive a file descriptor in return.
    let txt_path_cstring = CString::new(txt_path.to_str().unwrap()).unwrap();
    let params = door_h::door_arg_t {
        data_ptr: txt_path_cstring.as_ptr(),
        data_size: txt_path_str.len() + 1, // Include space for the 0 byte
        desc_ptr: ptr::null(),
        desc_num: 0,
        rbuf: ptr::null(),
        rsize: 0,
    };

    // Call the door with the payload we defined above. Since we told it that
    // rbuf was null, memory for the return payload will be mapped into this
    // address space for us. In particular, rbuf will no longer be null and will
    // instead point to the new memory region.
    unsafe { door_h::door_call(client_door_fd, &params) };
    assert_ne!(params.rbuf, ptr::null());

    // Unpack the returned descriptor array into a slice of descriptors and
    // insure that its length is indeed 1.
    let door_desc_ts = unsafe {
        std::slice::from_raw_parts::<door_h::door_desc_t>(
            params.desc_ptr,
            params.desc_num.try_into().unwrap(),
        )
    };
    assert_eq!(door_desc_ts.len(), 1);

    // Create a std::fs::File object based on the returned filed descriptor.
    let d_data = &door_desc_ts[0].d_data;
    let d_desc = unsafe { d_data.d_desc };
    let raw_fd = d_desc.d_descriptor as RawFd;
    let mut txt = unsafe { std::fs::File::from_raw_fd(raw_fd) };

    // Read that file's contents (reading, ultimately, from whatever this
    // descriptor refers to) into a string and compare them against the expected
    // value that we wrong to the /tmp/barebones_open_server.txt file earlier.
    let mut buffer = String::new();
    txt.read_to_string(&mut buffer).expect("read txt");
    assert_eq!(&buffer, "Hello, World!\n");
}
