//! A Rust-friendlier door client

use crate::illumos::door_h::door_arg_t;
use crate::illumos::door_h::door_call;
use crate::illumos::errno_h::errno;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::illumos::door_h::door_arg_t;
    use std::os::fd::AsRawFd;
    use std::os::fd::IntoRawFd;

    #[test]
    fn call_door() {
        let text = b"Hello, World!";
        let mut buffer = [0; 1024];
        let mut args = door_arg_t::new(text, &vec![], &mut buffer);
        let file = std::fs::File::open("/tmp/barebones_server.door").unwrap();
        let door = unsafe { Client::from_raw_fd(file.as_raw_fd()) };

        door.call(&mut args).unwrap();
        assert_eq!(args.data_size, 13);
        let response = unsafe { std::ffi::CStr::from_ptr(args.data_ptr) };
        let response = response.to_str().unwrap();
        assert_eq!(response, "HELLO, WORLD!");
    }

    #[test]
    fn open_door_from_path() {
        let text = b"Hello, World!";
        let mut buffer = [0; 1024];
        let mut args = door_arg_t::new(text, &vec![], &mut buffer);
        let door = Client::open("/tmp/barebones_server.door").unwrap();

        door.call(&mut args).unwrap();
        assert_eq!(args.data_size, 13);
        let response = unsafe { std::ffi::CStr::from_ptr(args.data_ptr) };
        let response = response.to_str().unwrap();
        assert_eq!(response, "HELLO, WORLD!");
    }

    #[test]
    fn dropped_doors_are_invalid() {
        let text = b"Hello, World!";
        let mut buffer = [0; 1024];
        let mut args = door_arg_t::new(text, &vec![], &mut buffer);
        let file = std::fs::File::open("/tmp/barebones_server.door").unwrap();
        let fd = file.as_raw_fd();
        let door = unsafe { Client::from_raw_fd(file.into_raw_fd()) };

        door.call(&mut args).unwrap();
        assert_eq!(args.data_size, 13);
        let response = unsafe { std::ffi::CStr::from_ptr(args.data_ptr) };
        let response = response.to_str().unwrap();
        assert_eq!(response, "HELLO, WORLD!");

        drop(door);

        let door = unsafe { Client::from_raw_fd(fd) };
        let mut args = door_arg_t::new(text, &vec![], &mut buffer);
        assert_eq!(door.call(&mut args), Err(DoorCallError::EBADF));
    }
}
