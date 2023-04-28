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
    pub fn call(&self, arg: &mut door_arg_t) -> Result<(), DoorCallError> {
        match unsafe { door_call(self.0, arg) } {
            0 => Ok(()),
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
    ) -> Result<DoorArg, DoorCallError> {
        let mut arg = DoorArg::new(data, &[], &mut []);
        self.call(arg.as_mut_door_arg_t())?;
        Ok(arg)
    }
}

pub struct DoorPayload<'a, 'b> {
    data: &'a [u8],
    descriptors: Vec<illumos::DoorFd>,
    original_rbuf: Option<&'a mut [u8]>,
    mmaped_rbuf: Option<&'b mut [u8]>,
}

impl<'a, 'b> DoorPayload<'a, 'b> {
    pub fn new(data: &'a [u8]) -> Self {
        let descriptors = vec![];
        let original_rbuf = None;
        let mmaped_rbuf = None;
        Self {
            data,
            descriptors,
            original_rbuf,
            mmaped_rbuf,
        }
    }

    pub fn new_with_rbuf(data: &'a [u8], rbuf: &'a mut [u8]) -> Self {
        let descriptors = vec![];
        let original_rbuf = Some(rbuf);
        let mmaped_rbuf = None;
        Self {
            data,
            descriptors,
            original_rbuf,
            mmaped_rbuf,
        }
    }

    fn as_door_arg(&mut self) -> DoorArg {
        let data = self.data;
        let descriptors = &self.descriptors;
        if let Some(buf) = &mut self.mmaped_rbuf {
            return DoorArg::new(data, descriptors, buf);
        }
        if let Some(buf) = &mut self.original_rbuf {
            return DoorArg::new(data, descriptors, buf);
        }
        DoorArg::new(data, descriptors, &mut [])
    }

    pub fn call(&mut self, client: Client) -> Result<(), DoorCallError> {
        let mut binding = self.as_door_arg();
        let arg = binding.as_mut_door_arg_t();
        client.call(arg)?;
        if arg.rbuf == (self.data.as_ptr() as *const i8) {
            self.mmaped_rbuf = None; // IS this a leak?
        } else {
            self.mmaped_rbuf = Some(unsafe {
                std::slice::from_raw_parts_mut(arg.rbuf as *mut u8, arg.rsize)
            });
        }

        Ok(())
    }
}

impl<'a, 'b> Drop for DoorPayload<'a, 'b> {
    fn drop(&mut self) {
        if let Some(x) = &mut self.mmaped_rbuf {
            unsafe {
                libc::munmap(x.as_mut_ptr() as *mut libc::c_void, x.len());
            }
        }
    }
}
