// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The one place in `doors` that talks to `doors-sys`.
//!
//! # Why this module exists
//!
//! Every raw call the crate makes goes through here. So there is one
//! file to read to see exactly which system calls this crate makes,
//! and one file to change when a call needs a different signature
//! from the raw C one.
//!
//! Two other things live here for the same reason:
//!
//! - [`last_errno`], because "the callee left errno at zero" needs
//!   one answer, not a different one at each call site.
//! - The `ucred_*` accessors. Those belong to `libucred`, not to the
//!   doors API, so `doors-sys` does not declare them. This module
//!   does.
//!
//! Doors are an illumos facility, so this crate is illumos only.
//! Nothing here is gated by platform: it builds on illumos, or it
//! does not build.

use crate::types::{door_arg_t, door_desc_t, door_info_t};
use doors_sys::Errno;
use std::ffi::{c_char, c_int, c_uint, c_void};

/// Read this thread's errno, as a value that cannot be zero.
///
/// A zero errno after a failed call means the callee did not set one.
/// We report `EINVAL` rather than claim success; see
/// `experiments/README.md`, where `door_return` is observed doing
/// exactly that.
pub(crate) fn last_errno() -> Errno {
    let raw = doors_sys::errno();
    match Errno::new(raw) {
        Some(e) => e,
        // EINVAL is a real errno, so this cannot collide with "no
        // error"; the type simply forbids representing zero.
        None => Errno::new(libc::EINVAL).expect("EINVAL is non-zero"),
    }
}

/// `door_create(3C)`.
pub(crate) unsafe fn door_create(
    proc_: crate::types::ServerProcedure,
    cookie: *mut c_void,
    attrs: c_uint,
) -> c_int {
    doors_sys::door_create(proc_, cookie, attrs)
}

/// `door_call(3C)`.
pub(crate) unsafe fn door_call(d: c_int, arg: *mut door_arg_t) -> c_int {
    doors_sys::door_call(d, arg)
}

/// `door_server_create(3C)`.
pub(crate) unsafe fn door_server_create(
    f: Option<crate::types::ServerThreadFunc>,
) -> Option<crate::types::ServerThreadFunc> {
    doors_sys::door_server_create(f)
}

/// `door_revoke(3C)`.
pub(crate) unsafe fn door_revoke(d: c_int) -> c_int {
    doors_sys::door_revoke(d)
}

/// `door_info(3C)`.
pub(crate) unsafe fn door_info(d: c_int, info: *mut door_info_t) -> c_int {
    doors_sys::door_info(d, info)
}

/// `door_getparam(3C)`.
pub(crate) unsafe fn door_getparam(
    d: c_int,
    param: c_int,
    out: *mut usize,
) -> c_int {
    doors_sys::door_getparam(d, param, out)
}

/// `door_setparam(3C)`.
pub(crate) unsafe fn door_setparam(
    d: c_int,
    param: c_int,
    val: usize,
) -> c_int {
    doors_sys::door_setparam(d, param, val)
}

/// `door_return(3C)`. Only the trampoline may call this.
pub(crate) unsafe fn door_return(
    data: *const c_char,
    data_size: usize,
    desc: *const door_desc_t,
    ndesc: c_uint,
) -> Errno {
    doors_sys::door_return(data, data_size, desc, ndesc)
}

/// `fattach(3C)`.
pub(crate) unsafe fn fattach(fd: c_int, path: *const c_char) -> c_int {
    doors_sys::fattach(fd, path)
}

/// `fdetach(3C)`.
pub(crate) unsafe fn fdetach(path: *const c_char) -> c_int {
    doors_sys::fdetach(path)
}

/// `pthread_atfork(3C)`.
pub(crate) unsafe fn pthread_atfork(
    prepare: Option<extern "C" fn()>,
    parent: Option<extern "C" fn()>,
    child: Option<extern "C" fn()>,
) -> c_int {
    doors_sys::pthread_atfork(prepare, parent, child)
}

/// `pthread_setcancelstate(3C)`.
pub(crate) unsafe fn pthread_setcancelstate(
    state: c_int,
    old: *mut c_int,
) -> c_int {
    doors_sys::pthread_setcancelstate(state, old)
}

/// `thr_min_stack(3C)`.
pub(crate) unsafe fn thr_min_stack() -> usize {
    doors_sys::thr_min_stack()
}

/// `door_ucred(3C)`. The `*mut *mut c_void` is a `ucred_t **`.
///
/// A `ucred_t` is opaque: the only legal way to read one is through
/// the `ucred_*` accessors below. Passing it around as `*mut c_void`
/// says that out loud, and keeps a libc type out of the fields of
/// [`Request`](crate::Request).
pub(crate) unsafe fn door_ucred(out: *mut *mut c_void) -> c_int {
    doors_sys::door_ucred(out as *mut *mut libc::ucred_t)
}

/// `ucred_free(3C)`.
pub(crate) unsafe fn ucred_free(u: *mut c_void) {
    extern "C" {
        fn ucred_free(u: *mut libc::ucred_t);
    }
    ucred_free(u as *mut libc::ucred_t)
}

/// `ucred_geteuid(3C)`.
pub(crate) unsafe fn ucred_geteuid(u: *mut c_void) -> libc::uid_t {
    extern "C" {
        fn ucred_geteuid(u: *const libc::ucred_t) -> libc::uid_t;
    }
    ucred_geteuid(u as *const libc::ucred_t)
}

/// `ucred_getegid(3C)`.
pub(crate) unsafe fn ucred_getegid(u: *mut c_void) -> libc::gid_t {
    extern "C" {
        fn ucred_getegid(u: *const libc::ucred_t) -> libc::gid_t;
    }
    ucred_getegid(u as *const libc::ucred_t)
}

/// `ucred_getruid(3C)`.
pub(crate) unsafe fn ucred_getruid(u: *mut c_void) -> libc::uid_t {
    extern "C" {
        fn ucred_getruid(u: *const libc::ucred_t) -> libc::uid_t;
    }
    ucred_getruid(u as *const libc::ucred_t)
}

/// `ucred_getrgid(3C)`.
pub(crate) unsafe fn ucred_getrgid(u: *mut c_void) -> libc::gid_t {
    extern "C" {
        fn ucred_getrgid(u: *const libc::ucred_t) -> libc::gid_t;
    }
    ucred_getrgid(u as *const libc::ucred_t)
}

/// `ucred_getpid(3C)`.
pub(crate) unsafe fn ucred_getpid(u: *mut c_void) -> libc::pid_t {
    extern "C" {
        fn ucred_getpid(u: *const libc::ucred_t) -> libc::pid_t;
    }
    ucred_getpid(u as *const libc::ucred_t)
}
