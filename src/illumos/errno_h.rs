//! Rust translation of illumos' `errno.h` header file
//!
//! This module merely re-exports the subset of the errno api that we
//! need for this project. It makes no attempt at safety or ergonomics.
use libc;

/// Good ole UNIX errno
///
/// `errno` is implemented in the libc crate, sortof. In real life, it's
/// allegedly a macro or something. Can't be bothered to look it up. Point is,
/// once we've done a goof, we call this to figure out which goof we've done.
///
/// See [`PERROR(3C)`], but don't think too hard about the fact that this is a
/// function and that one doesn't seem to be.
///
/// [`PERROR(3C)`]: https://illumos.org/man/3c/errno
pub fn errno() -> libc::c_int {
    unsafe { *libc::___errno() }
}
