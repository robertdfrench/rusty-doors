// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Types from `<sys/door.h>`.
//!
//! # Packing
//!
//! `<sys/door.h>` wraps [`door_desc_t`] and [`door_info_t`] in
//! `#pragma pack(4)`:
//!
//! ```c
//! #if _LONG_LONG_ALIGNMENT == 8 && _LONG_LONG_ALIGNMENT_32 == 4
//! #pragma pack(4)
//! #endif
//! ```
//!
//! That guard holds on amd64, so both structs are 4-aligned there and
//! their 8-byte members sit at offsets that are *not* 8-aligned:
//! `door_info_t.di_proc` lands at offset 4 and
//! `door_desc_t.d_data.d_desc.d_id` at offset 8. Reproducing this with
//! plain `#[repr(C)]` would silently misplace every field after the
//! first, so both carry `#[repr(C, packed(4))]`.
//!
//! [`door_arg_t`] is declared *outside* that guard and is ordinarily
//! aligned.

use core::ffi::{c_int, c_uint, c_void};
use libc::{gid_t, pid_t, size_t, uid_t};

/// Handle 64-bit pointers. `unsigned long long` in C.
#[allow(non_camel_case_types)]
pub type door_ptr_t = u64;

/// Unique door identifier. `unsigned long long` in C.
#[allow(non_camel_case_types)]
pub type door_id_t = u64;

/// Door attributes. `unsigned int` in C.
#[allow(non_camel_case_types)]
pub type door_attr_t = c_uint;

/// Thread identifier. `<sys/types.h>` defines `pthread_t` as `uint_t`
/// and notes it is "= thread_t in thread.h"; the two are the same type
/// on illumos.
#[allow(non_camel_case_types)]
pub type thread_t = c_uint;

// ---------------------------------------------------------------------
// door_desc_t
// ---------------------------------------------------------------------

/// The `d_desc` arm of [`door_desc_data`].
#[allow(non_camel_case_types)]
#[repr(C, packed(4))]
#[derive(Copy, Clone)]
pub struct door_desc_desc {
    /// The file descriptor being passed.
    pub d_descriptor: c_int,
    /// Unique id, set by the kernel when the descriptor is a door.
    pub d_id: door_id_t,
}

/// The `d_data` union of [`door_desc_t`].
///
/// The `d_resv[5]` arm is what fixes the union's size at 20 bytes; the
/// `d_desc` arm only occupies 12. Omitting it would make every
/// `door_desc_t` in an array land at the wrong offset.
#[allow(non_camel_case_types)]
#[repr(C, packed(4))]
#[derive(Copy, Clone)]
pub union door_desc_data {
    /// A file descriptor is being passed.
    pub d_desc: door_desc_desc,
    /// Reserved space. Sizes the union.
    pub d_resv: [c_int; 5],
}

/// Structure used to pass descriptors/objects in door invocations.
///
/// ```c
/// typedef struct door_desc {
///         door_attr_t     d_attributes;
///         union {
///                 struct {
///                         int             d_descriptor;
///                         door_id_t       d_id;
///                 } d_desc;
///                 int     d_resv[5];
///         } d_data;
/// } door_desc_t;
/// ```
#[allow(non_camel_case_types)]
#[repr(C, packed(4))]
#[derive(Copy, Clone)]
pub struct door_desc_t {
    /// Tag for the union.
    pub d_attributes: door_attr_t,
    /// The descriptor itself, or reserved space.
    pub d_data: door_desc_data,
}

// ---------------------------------------------------------------------
// door_info_t
// ---------------------------------------------------------------------

/// Structure used to return info from [`door_info`](crate::door_info).
#[allow(non_camel_case_types)]
#[repr(C, packed(4))]
#[derive(Copy, Clone)]
pub struct door_info_t {
    /// Server process.
    pub di_target: pid_t,
    /// Server procedure.
    pub di_proc: door_ptr_t,
    /// Data cookie.
    pub di_data: door_ptr_t,
    /// Attributes associated with the door.
    pub di_attributes: door_attr_t,
    /// Unique number.
    pub di_uniquifier: door_id_t,
    /// Future use.
    pub di_resv: [c_int; 4],
}

// ---------------------------------------------------------------------
// door_arg_t
// ---------------------------------------------------------------------

/// Structure used to pass/return data from
/// [`door_call`](crate::door_call).
///
/// Every field is an in/out parameter: on return they describe where
/// the results actually landed, which is not necessarily `rbuf`.
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Copy, Clone)]
pub struct door_arg_t {
    /// Argument/result data.
    pub data_ptr: *mut core::ffi::c_char,
    /// Argument/result data size.
    pub data_size: size_t,
    /// Argument/result descriptors.
    pub desc_ptr: *mut door_desc_t,
    /// Argument/result descriptor count.
    pub desc_num: c_uint,
    /// Result area.
    pub rbuf: *mut core::ffi::c_char,
    /// Result area size.
    pub rsize: size_t,
}

// ---------------------------------------------------------------------
// door_cred_t
// ---------------------------------------------------------------------

/// Structure used to return info from [`door_cred`](crate::door_cred).
///
/// `door_cred(3C)` is obsolete; prefer
/// [`door_ucred`](crate::door_ucred).
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Copy, Clone)]
pub struct door_cred_t {
    /// Effective uid of the client.
    pub dc_euid: uid_t,
    /// Effective gid of the client.
    pub dc_egid: gid_t,
    /// Real uid of the client.
    pub dc_ruid: uid_t,
    /// Real gid of the client.
    pub dc_rgid: gid_t,
    /// pid of the client.
    pub dc_pid: pid_t,
    /// Future use.
    pub dc_resv: [c_int; 4],
}

// ---------------------------------------------------------------------
// door_return_desc_t
// ---------------------------------------------------------------------

/// Structure used to pass a descriptor list to
/// [`door_return`](crate::door_return).
#[allow(non_camel_case_types)]
#[repr(C)]
#[derive(Copy, Clone)]
pub struct door_return_desc_t {
    /// The descriptors.
    pub desc_ptr: *mut door_desc_t,
    /// How many.
    pub desc_num: c_uint,
}

// ---------------------------------------------------------------------
// Function types
// ---------------------------------------------------------------------

/// `typedef void door_server_procedure_t(void *, char *, size_t,
/// door_desc_t *, uint_t);`
#[allow(non_camel_case_types)]
pub type door_server_procedure_t = unsafe extern "C" fn(
    cookie: *mut c_void,
    argp: *mut core::ffi::c_char,
    arg_size: size_t,
    dp: *mut door_desc_t,
    n_desc: c_uint,
);

/// `typedef void door_server_func_t(door_info_t *);`
#[allow(non_camel_case_types)]
pub type door_server_func_t = unsafe extern "C" fn(info: *mut door_info_t);

/// The `void *(*)(void *)` a
/// [`door_xcreate_server_func_t`] is handed to start a thread with.
#[allow(non_camel_case_types)]
pub type door_xcreate_thrfunc_t =
    unsafe extern "C" fn(arg: *mut c_void) -> *mut c_void;

/// `typedef int door_xcreate_server_func_t(door_info_t *,
/// void *(*)(void *), void *, void *);`
#[allow(non_camel_case_types)]
pub type door_xcreate_server_func_t = unsafe extern "C" fn(
    info: *mut door_info_t,
    thrfunc: door_xcreate_thrfunc_t,
    thrarg: *mut c_void,
    cookie: *mut c_void,
) -> c_int;

/// `typedef void door_xcreate_thrsetup_func_t(void *);`
#[allow(non_camel_case_types)]
pub type door_xcreate_thrsetup_func_t =
    unsafe extern "C" fn(cookie: *mut c_void);
