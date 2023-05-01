/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Copyright 2023 Robert D. French
 */
//! A Rust-friendly interface for [illumos Doors][1].
//!
//! [Doors][2] are a high-speed, RPC-style interprocess communication facility
//! for the [illumos][3] operating system. They enable rapid dialogue between
//! client and server without giving up the CPU timeslice, and are an excellent
//! alternative to pipes or UNIX domain sockets in situations where IPC latency
//! matters.
//!
//! This crate makes it easier to interact with the Doors API from Rust. It can
//! help you create clients, define server procedures, and open or create doors
//! on the filesystem.
//!
//! ## Example
//! ```
//! // In the Server --------------------------------------- //
//! use doors::server::Door;
//! use doors::server::Request;
//! use doors::server::Response;
//!
//! #[doors::server_procedure]
//! fn double(x: Request) -> Response<[u8; 1]> {
//!   if x.data.len() > 0 {
//!     return Response::new([x.data[0] * 2]);
//!   } else {
//!     // We were given nothing, and 2 times nothing is zero...
//!     return Response::new([0]);
//!   }
//! }
//!
//! let door = Door::create(double).unwrap();
//! door.force_install("/tmp/double.door").unwrap();
//!
//! // In the Client --------------------------------------- //
//! use doors::Client;
//!
//! let client = Client::open("/tmp/double.door").unwrap();
//!
//! let response = client.call_with_data(&[111]).unwrap();
//! assert_eq!(response.data()[0], 222);
//! ```
//!
//! [1]: https://github.com/robertdfrench/revolving-doors
//! [2]: https://illumos.org/man/3C/door_create
//! [3]: https://illumos.org
pub use door_macros::server_procedure;

pub mod illumos;
pub mod server;

use crate::illumos::door_h::door_arg_t;
use crate::illumos::door_h::door_call;
use crate::illumos::errno_h::errno;
use crate::illumos::DoorArg;
use crate::illumos::DoorFd;
use std::fs::File;
use std::io;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::RawFd;
use std::path::Path;

/// Failure conditions for [`door_call`].
///
/// According to [`door_call(3C)`], if a [`door_call`] fails, errno will be set
/// to one of these values. While this enum is not strictly derived from
/// anything in [doors.h][1], it is spelled out in the man page.
///
/// [`door_call(3C)`]: https://illumos.org/man/3C/door_call
/// [1]: https://github.com/illumos/illumos-gate/blob/master/usr/src/uts/common/sys/door.h
#[derive(Debug, PartialEq)]
pub enum DoorCallError {
    /// Arguments were too big for server thread stack.
    E2BIG,

    /// Server was out of available resources.
    EAGAIN,

    /// Invalid door descriptor was passed.
    EBADF,

    /// Argument pointers pointed outside the allocated address space.
    EFAULT,

    /// A signal was caught in the client, the client called [`fork(2)`], or the
    /// server exited during invocation.
    ///
    /// [`fork(2)`]: https://illumos.org/man/2/fork
    EINTR,

    /// Bad arguments were passed.
    EINVAL,

    /// The client or server has too many open descriptors.
    EMFILE,

    /// The desc_num argument is larger than the door's `DOOR_PARAM_DESC_MAX`
    /// parameter (see [`door_getparam(3C)`]), and the door does not have the
    /// [`DOOR_REFUSE_DESC`][crate::illumos::door_h::DOOR_REFUSE_DESC] set.
    ///
    /// [`door_getparam(3C)`]: https://illumos.org/man/3C/door_getparam
    ENFILE,

    /// The data_size argument is larger than the door's `DOOR_PARAM_DATA_MAX`
    /// parameter, or smaller than the door's `DOOR_PARAM_DATA_MIN` parameter
    /// (see [`door_getparam(3C)`]).
    ///
    /// [`door_getparam(3C)`]: https://illumos.org/man/3C/door_getparam
    ENOBUFS,

    /// The desc_num argument is non-zero and the door has the
    /// [`DOOR_REFUSE_DESC`][crate::illumos::door_h::DOOR_REFUSE_DESC] flag set.
    ENOTSUP,

    /// System could not create overflow area in caller for results.
    EOVERFLOW,
}

/// Less unsafe door client (compared to raw file descriptors)
///
/// Clients are automatically closed when they go out of scope. Errors detected
/// on closing are ignored by the implementation of `Drop`, just like in
/// [`File`].
pub struct Client(RawFd);

impl FromRawFd for Client {
    unsafe fn from_raw_fd(raw: RawFd) -> Self {
        Self(raw)
    }
}

impl Drop for Client {
    /// Automatically close the door on your way out.
    ///
    /// This will close the file descriptor associated with this door, so that
    /// this process will no longer be able to call this door. For that reason,
    /// it is a programming error to [`Clone`] this type.
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

pub enum DoorArgument {
    BorrowedRbuf(DoorArg),
    OwnedRbuf(DoorArg),
}

impl DoorArgument {
    pub fn new(
        data: &[u8],
        descriptors: &[DoorFd],
        response: &mut [u8],
    ) -> Self {
        Self::borrowed_rbuf(data, descriptors, response)
    }

    pub fn borrowed_rbuf(
        data: &[u8],
        descriptors: &[DoorFd],
        response: &mut [u8],
    ) -> Self {
        Self::BorrowedRbuf(DoorArg::new(data, descriptors, response))
    }

    pub fn owned_rbuf(
        data: &[u8],
        descriptors: &[DoorFd],
        response: &mut [u8],
    ) -> Self {
        Self::OwnedRbuf(DoorArg::new(data, descriptors, response))
    }

    fn inner(&self) -> &DoorArg {
        match self {
            Self::BorrowedRbuf(inner) => inner,
            Self::OwnedRbuf(inner) => inner,
        }
    }

    fn inner_mut(&mut self) -> &mut DoorArg {
        match self {
            Self::BorrowedRbuf(inner) => inner,
            Self::OwnedRbuf(inner) => inner,
        }
    }

    pub fn as_door_arg_t(&self) -> &door_arg_t {
        self.inner().as_door_arg_t()
    }

    pub fn data(&self) -> &[u8] {
        self.inner().data()
    }

    pub fn rbuf(&self) -> &[u8] {
        self.inner().rbuf()
    }
}

impl Drop for DoorArgument {
    fn drop(&mut self) {
        if let Self::OwnedRbuf(arg) = self {
            // If munmap fails, we do want to panic, because it means we've
            // tried to munmap something that wasn't mapped into our address
            // space. That should never happen, but if it does, it's worth
            // crashing, because something else is seriously wrong.
            arg.munmap_rbuf().unwrap()
        }
    }
}

impl Client {
    /// Open a door client like you would a file
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self(file.into_raw_fd()))
    }

    /// Issue a door call
    ///
    /// You are responsible for managing this memory. See [`DOOR_CALL(3C)`].
    /// Particularly, if, after a `door_call`, the `rbuf` property of
    /// [`door_arg_t`] is different than what it was before the `door_call`, you
    /// are responsible for reclaiming this area with [`MUNMAP(2)`] when you are
    /// done with it.
    ///
    /// This crate cannot yet handle this for you. See [Issue
    /// #11](https://github.com/robertdfrench/rusty-doors/issues/11).
    ///
    /// [`DOOR_CALL(3C)`]: https://illumos.org/man/3C/door_call
    /// [`MUNMAP(2)`]: https://illumos.org/man/2/munmap
    pub fn call(
        &self,
        mut arg: DoorArgument,
    ) -> Result<DoorArgument, DoorCallError> {
        let a = arg.inner().rbuf_addr();
        let x = arg.inner_mut().as_mut_door_arg_t();
        match unsafe { door_call(self.0, x) } {
            0 => match (x.rbuf as u64) == a {
                true => Ok(arg),
                false => {
                    let data = unsafe {
                        std::slice::from_raw_parts(
                            x.data_ptr as *const u8,
                            x.data_size,
                        )
                    };
                    let desc = unsafe {
                        std::slice::from_raw_parts(
                            x.desc_ptr as *const DoorFd,
                            x.desc_num.try_into().unwrap(),
                        )
                    };
                    let rbuf = unsafe {
                        std::slice::from_raw_parts_mut(
                            x.rbuf as *mut u8,
                            x.rsize,
                        )
                    };
                    Ok(DoorArgument::owned_rbuf(data, desc, rbuf))
                }
            },
            _ => Err(match errno() {
                libc::E2BIG => DoorCallError::E2BIG,
                libc::EAGAIN => DoorCallError::EAGAIN,
                libc::EBADF => DoorCallError::EBADF,
                libc::EFAULT => DoorCallError::EFAULT,
                libc::EINTR => DoorCallError::EINTR,
                libc::EINVAL => DoorCallError::EINVAL,
                libc::EMFILE => DoorCallError::EMFILE,
                libc::ENFILE => DoorCallError::ENFILE,
                libc::ENOBUFS => DoorCallError::ENOBUFS,
                libc::ENOTSUP => DoorCallError::ENOTSUP,
                libc::EOVERFLOW => DoorCallError::EOVERFLOW,
                _ => unreachable!(),
            }),
        }
    }

    /// Issue a door call with Data only
    ///
    /// ## Example
    ///
    /// ```rust
    /// use doors::Client;
    /// use std::ffi::CString;
    /// use std::ffi::CStr;
    ///
    /// let capitalize = Client::open("/tmp/barebones_capitalize.door")
    ///     .unwrap();
    /// let text = CString::new("Hello, World!").unwrap();
    /// let response = capitalize.call_with_data(text.as_bytes()).unwrap();
    /// let caps = unsafe {
    ///     CStr::from_ptr(response.data().as_ptr() as *const i8)
    /// };
    /// assert_eq!(caps.to_str(), Ok("HELLO, WORLD!"));
    /// ```
    pub fn call_with_data(
        &self,
        data: &[u8],
    ) -> Result<DoorArgument, DoorCallError> {
        let arg = DoorArgument::new(data, &[], &mut []);
        self.call(arg)
    }
}
