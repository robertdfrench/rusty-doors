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

use std::ops::BitOr;
use std::ops::BitOrAssign;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// illumos Error Conditions
///
/// These are the values that `errno` can return, but presented as a
/// Rust-friendly enumeration.
#[derive(Debug, PartialEq)]
pub enum Error {
    /// The user is the owner of path but does not have write
    /// permissions on path or fildes is locked.
    EACCES,

    /// The fildes argument is not a valid open file descriptor.
    EBADF,

    /// The path argument is currently a mount point or has a doors file
    /// descriptor attached to it.
    EBUSY,

    /// Invalid Arguments
    ///
    /// * `fattach` - The path argument is a file in a remotely mounted directory.
    ///   Alternatively, the fildes argument does not represent a doors file.
    /// * `door_create` - invalid attributes were passed
    EINVAL,

    /// Too many symbolic links were encountered in translating path.
    ELOOP,

    /// The process has too many open descriptors.
    EMFILE,

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

    /// Bad address
    EFAULT,
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

/// Raw, Unvarnished Server Procedure
///
/// This is a function that literally matches the signature given in
/// [`DOOR_CREATE(3C)`]. It is either written by hand, or generated from a trait
/// or a macro, but it is not a function to which a trait or a macro is applied.
///
/// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3C/door_create
pub type ServerProcedure = door_h::door_server_procedure_t;

/// Change a door's behavior
#[derive(Debug, PartialEq)]
pub struct DoorAttributes {
    attrs: u32,
}

impl DoorAttributes {
    pub fn none() -> Self {
        Self { attrs: 0 }
    }

    pub fn unref() -> Self {
        Self {
            attrs: door_h::DOOR_UNREF,
        }
    }

    pub fn unref_multi() -> Self {
        Self {
            attrs: door_h::DOOR_UNREF_MULTI,
        }
    }

    pub fn private() -> Self {
        Self {
            attrs: door_h::DOOR_PRIVATE,
        }
    }

    pub fn refuse_desc() -> Self {
        Self {
            attrs: door_h::DOOR_REFUSE_DESC,
        }
    }

    pub fn no_cancel() -> Self {
        Self {
            attrs: door_h::DOOR_NO_CANCEL,
        }
    }

    pub fn get(&self) -> u32 {
        self.attrs
    }
}

impl BitOr for DoorAttributes {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self {
            attrs: self.attrs | rhs.attrs,
        }
    }
}

impl BitOrAssign for DoorAttributes {
    fn bitor_assign(&mut self, rhs: Self) {
        self.attrs |= rhs.attrs;
    }
}

/// Create a door descriptor from a server procedure and a cookie.
///
/// See [`DOOR_CREATE(3C)`] for more details.
///
/// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3C/door_create
pub fn door_create(
    server_procedure: ServerProcedure,
    cookie: u64,
    attributes: DoorAttributes,
) -> Result<RawFd, Error> {
    let result = unsafe {
        door_h::door_create(
            server_procedure,
            cookie as *const libc::c_void,
            attributes.get(),
        )
    };
    match result {
        -1 => match errno_h::errno() {
            libc::EINVAL => Err(Error::EINVAL),
            libc::EMFILE => Err(Error::EMFILE),
            _ => unreachable!(),
        },
        fd => Ok(fd as RawFd),
    }
}

#[derive(Debug, PartialEq)]
pub struct DoorInfo(door_h::door_info_t);

pub fn door_info(fd: RawFd) -> Result<DoorInfo, Error> {
    let mut info: door_h::door_info_t = Default::default();
    match unsafe { door_h::door_info(fd, &mut info) } {
        0 => Ok(DoorInfo(info)),
        _ => match errno_h::errno() {
            libc::EFAULT => Err(Error::EFAULT),
            libc::EBADF => Err(Error::EBADF),
            _ => unreachable!(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libc;
    use std::ffi::CString;

    #[test]
    fn errno_works() {
        // This test will purposefully open a nonexistant file via the libc
        // crate, and then check that errno is the expected value.
        let badpath = CString::new("<(^_^)>").unwrap();
        assert_eq!(unsafe { libc::open(badpath.as_ptr(), libc::O_RDONLY) }, -1);
        assert_eq!(errno_h::errno(), libc::ENOENT);
    }

    #[test]
    fn door_info_error() {
        let e = door_info(-1);
        assert_eq!(e, Err(Error::EBADF));
    }
}
