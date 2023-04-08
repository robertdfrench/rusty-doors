/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Copyright 2021 Robert D. French
 */

//! illumos-specific APIs not (yet) found in the libc crate
//!
//! In this module, we represent only the subset of the illumos-specific APIs
//! that we need for creating and invoking doors, and for advertising them on
//! the filesystem.

pub mod door_h;
pub mod errno_h;
pub mod stropts_h;

use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub enum Error {
    /// The user is the owner of path but does not have write
    /// permissions on path or fildes is locked.
    EACCES,

    /// The fildes argument is not a valid open file descriptor.
    EBADF,

    /// The path argument is currently a mount point or has a doors file
    /// descriptor attached to it.
    EBUSY,

    /// The path argument is a file in a remotely mounted directory.
    /// Alternatively, the fildes argument does not represent a doors file.
    EINVAL,

    /// Too many symbolic links were encountered in translating path.
    ELOOP,

    /// The size of path exceeds `{PATH_MAX}`, or the component of a path name
    /// is longer than `{NAME_MAX}` while `{_POSIX_NO_TRUNC}` is in effect.
    ENAMETOOLONG,

    /// The path argument does not exist.
    ENOENT,

    /// A component of a path prefix is not a directory.
    ENOTDIR,

    /// The effective user ID is not the owner of path or a
    /// user with the appropriate privileges.
    EPERM,
}

/// Attach a doors-based file descriptor to an object in the file system name
/// space.
///
/// See [`FATTACH(3C)`] for more details.
///
/// [`FATTACH(3C)`]: https://illumos.org/man/3C/fattach
pub fn fattach<P: AsRef<Path>>(fildes: RawFd, path: P) -> Result<(), Error> {
    let path_bytes = path.as_ref().as_os_str().as_bytes();
    // TODO: Why is it safe to unwrap here?
    let c_string = std::ffi::CString::new(path_bytes).unwrap();
    match unsafe { stropts_h::fattach(fildes, c_string.as_ptr()) } {
        0 => Ok(()),
        _ => match errno_h::errno() {
            libc::EACCES => Err(Error::EACCES),
            libc::EBADF => Err(Error::EBADF),
            libc::EBUSY => Err(Error::EBUSY),
            libc::EINVAL => Err(Error::EINVAL),
            libc::ELOOP => Err(Error::ELOOP),
            libc::ENAMETOOLONG => Err(Error::ENAMETOOLONG),
            libc::ENOENT => Err(Error::ENOENT),
            libc::ENOTDIR => Err(Error::ENOTDIR),
            libc::EPERM => Err(Error::EPERM),
            _ => unreachable!(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libc;
    use std::ffi::{CStr, CString};
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::FromRawFd;
    use std::os::fd::RawFd;
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
    fn can_invoke_own_door_twice() {
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
    fn can_receive_file_descriptor() {
        let door_path = Path::new("/tmp/barebones_open_server.door");
        let door_path_cstring =
            CString::new(door_path.to_str().unwrap()).unwrap();

        let txt_path = Path::new("/tmp/barebones_open_server.txt");
        let mut txt = std::fs::File::create(txt_path).expect("create txt");
        writeln!(txt, "Hello, World!").expect("write txt");
        drop(txt);
        let txt_path_cstring =
            CString::new(txt_path.to_str().unwrap()).unwrap();

        // Connect to the Capitalization Server through its door.
        let client_door_fd =
            unsafe { libc::open(door_path_cstring.as_ptr(), libc::O_RDONLY) };

        // Pass `original` through the Capitalization Server's door.
        let data_ptr = txt_path_cstring.as_ptr();
        let data_size = 31;
        let desc_ptr = ptr::null();
        let desc_num = 0;
        let rbuf = unsafe { libc::malloc(data_size) as *mut libc::c_char };
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
        unsafe { door_h::door_call(client_door_fd, &params) };

        // Unpack the returned bytes and compare!
        let door_desc_ts = unsafe {
            std::slice::from_raw_parts::<door_h::door_desc_t>(
                params.desc_ptr,
                params.desc_num.try_into().unwrap(),
            )
        };
        assert_eq!(door_desc_ts.len(), 1);

        let d_data = &door_desc_ts[0].d_data;
        let d_desc = unsafe { d_data.d_desc };
        let raw_fd = d_desc.d_descriptor as RawFd;
        let mut txt = unsafe { std::fs::File::from_raw_fd(raw_fd) };
        let mut buffer = String::new();
        txt.read_to_string(&mut buffer).expect("read txt");
        assert_eq!(&buffer, "Hello, World!\n");

        // We did a naughty and called malloc, so we need to clean up. A PR
        // for a Rustier way to do this would be considered a personal
        // favor.
        unsafe { libc::free(rbuf as *mut libc::c_void) };
    }
}
