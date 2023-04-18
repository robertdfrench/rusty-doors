/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Copyright 2021 Robert D. French
 */
//! Unsafe Declarations for the illumos Doors API
//!
//! This module merely re-exports the subset of the illumos doors api that we
//! need for this project. It makes no attempt at safety or ergonomics. Insofar
//! as possible, all of the definitions provided here are verbatim Rust imports of the
//! definitions provided in /usr/include/sys/door.h
//!
//! Check out [revolving-doors][1] for an introduction to doors.
//!
//! [1]: https://github.com/robertdfrench/revolving-door#revolving-doors

#![allow(non_camel_case_types)]
use libc;

/// Signature for a Door Server Procedure
///
/// All "Server Procedures" (functions which respond to `door_call` requests)
/// must use this type signature. It accepts five arguments:
///
/// * `cookie` - a pointer to some (likely static) data. This is the same value
/// that is made available to the [`door_call`] function.
/// * `argp` - a pointer to a data region
/// * `arg_size` - the length, in bytes, of that data region
/// * `dp` - a pointer to an array of [`door_desc_t`] objects
/// * `n_desc` - the number of [`door_desc_t`] objects in that array.
///
/// See [`DOOR_CREATE(3C)`] for examples and further detail.
///
/// ## Examples
///
/// ### A Server Procedure Taking No Arguments
/// ```rust
/// use doors::illumos::door_h;
/// use std::ptr;
///
/// extern "C" fn hello_no_args(
///     _cookie: *const libc::c_void,
///     _argp: *const libc::c_char,
///     _arg_size: libc::size_t,
///     _dp: *const door_h::door_desc_t,
///     _n_desc: libc::c_uint,
/// ) {
///     println!("Hello, world!");
///     unsafe { door_h::door_return(ptr::null(), 0, ptr::null(), 0) };
/// }
/// ```
///
///
/// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3c/door_create
pub type door_server_procedure_t = extern "C" fn(
    cookie: *const libc::c_void,
    argp: *const libc::c_char,
    arg_size: libc::size_t,
    dp: *const door_desc_t,
    n_desc: libc::c_uint,
);

extern "C" {
    /// Turns a function into a file descriptor.
    ///
    /// The function in question must match the "Server Procedure" signature
    /// [door_server_procedure_t][1]. Portunus does not currently use the
    /// `cookie` argument. Since it will not send any file descriptors,
    /// applications are free to set `attributes` to
    /// [DOOR_REFUSE_DESC](constant.DOOR_REFUSE_DESC.html).
    ///
    /// See [`DOOR_CREATE(3C)`] for more details.
    ///
    /// [1]: type.door_server_procedure_t.html
    /// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3c/door_create
    pub fn door_create(
        server_procedure: door_server_procedure_t,
        cookie: *const libc::c_void,
        attributes: door_attr_t,
    ) -> libc::c_int;

    /// Invoke a function in another process.
    ///
    /// Assuming `d` is a descriptor for a door which points to a function in
    /// another process, this function can use an instance of [door_arg_t] to
    /// send data to and receive data from the function described by `d`.
    ///
    /// See [`DOOR_CALL(3C)`] for more details.
    ///
    /// [`DOOR_CALL(3C)`]: https://illumos.org/man/3c/door_call
    pub fn door_call(d: libc::c_int, params: *const door_arg_t) -> libc::c_int;

    /// The inverse of `door_call` - return data and control to the calling
    /// process.
    ///
    /// Use this at the end of `server_procedure` in lieu of the traditional
    /// `return` statement to transfer control back to the process which
    /// originally issued `door_call`. Like [`EXECVE(2)`], this function is
    /// terminal from the perspective of the code which calls it.
    ///
    /// See [`DOOR_RETURN(3C)`].
    ///
    /// # Warning
    ///
    /// It is [not yet clear][1] whether Rust structures are properly cleaned up
    /// upon `door_return`. Further, because threads (and thus their state) are
    /// re-used between requests, it is vitally important that any code calling
    /// `door_return` is able to purge sensitive stack data in order to hamper
    /// an attacker's ability to exfiltrate the data of other users.
    ///
    /// [`DOOR_RETURN(3C)`]: https://illumos.org/man/3c/door_return
    /// [`EXECVE(2)`]: https://illumos.org/man/2/execve
    /// [1]: https://github.com/robertdfrench/portunusd/issues/6
    pub fn door_return(
        data_ptr: *const libc::c_char,
        data_size: libc::size_t,
        desc_ptr: *const door_desc_t,
        num_desc: libc::c_uint,
    ) -> !;

    /// Return information associated with a door descriptor
    ///
    /// See [`DOOR_INFO(3C)`] for more information.
    ///
    /// [`DOOR_INFO(3C)`]: https://illumos.org/man/3c/door_info
    pub fn door_info(d: libc::c_int, info: &mut door_info_t) -> libc::c_int;

    /// Revoke access to a door descriptor
    ///
    /// See [`DOOR_REVOKE(3C)`] for more information.
    ///
    /// [`DOOR_REVOKE(3C)`]: https://illumos.org/man/3c/door_revoke
    pub fn door_revoke(d: libc::c_int) -> libc::c_int;
}

/// Arguments for, and Return Values from, a Door invocation.
///
/// This is your daily driver, right here. `data_ptr` and `data_size` represent
/// the bytes you want to send to the server. `rbuf` and `rsize` represent a
/// space you've set aside to store bytes that come back from the server; after
/// [`DOOR_CALL(3C)`] completes, `data_ptr` and `data_size` will bue updated to
/// point inside this space. `desc_ptr` and `desc_num` are for passing any file
/// / socket / door descriptors you'd like the server to be able to access. It
/// is described in more detail below.
///
/// See [`DOOR_CALL(3C)`] for more details.
///
/// [`DOOR_CALL(3C)`]: https://illumos.org/man/3c/door_call
#[derive(Debug)]
#[repr(C)]
pub struct door_arg_t {
    pub data_ptr: *const libc::c_char,
    pub data_size: libc::size_t,

    pub desc_ptr: *const door_desc_t,
    pub desc_num: libc::c_uint,

    pub rbuf: *const libc::c_char,
    pub rsize: libc::size_t,
}

/// Descriptor structure for [`door_arg_t`]
///
/// For our purposes, this data structure and its constituent parts are mostly
/// opaque *except* that it holds any file / socket / door descriptors which we
/// would like to pass between processes.  Rust does not support nested type
/// declaration like C does, so we define each component separately. See
/// [doors.h][1] for the original (nested) definition of this type and
/// [revolving-doors][2] for a visual guide.
///
/// [1]: https://github.com/illumos/illumos-gate/blob/master/usr/src/uts/common/sys/door.h#L122
/// [2]: https://github.com/robertdfrench/revolving-door/tree/master/A0_result_parameters
#[repr(C)]
pub struct door_desc_t {
    pub d_attributes: door_attr_t,
    pub d_data: door_desc_t__d_data,
}

/// Handling instructions for [`door_desc_t`]
///
/// Specified in the "Description" section of [`DOOR_CREATE(3C)`]. The file
/// descriptor enapsulated in a [`door_desc_t`] will need to be marked as a
/// [`DOOR_DESCRIPTOR`]. If the calling process should release this descriptor
/// to the receivng process, rather than *duplicating* it for the receiving
/// process, then it will also need to be maked with [`DOOR_RELEASE`].
///
/// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3c/door_create#DESCRIPTION
pub type door_attr_t = libc::c_uint;

/// Declare that a [`door_desc_t`] contains a file descriptor.
///
/// Specified in the "Description" section of [`DOOR_CREATE(3C)`], this flag
/// tells the illumos kernel that the associated [`door_desc_t`] object contains
/// a file descriptor. All [`door_desc_t`] objects must be marked with this
/// attribute,
///
/// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3c/door_create#DESCRIPTION
pub const DOOR_DESCRIPTOR: door_attr_t = 0x10000; // A file descriptor is being passed.

/// Instruct the kernel to close the descriptor after passing it to the server.
///
/// By default, file descriptors are *duplicated* into the receiving process.
/// But if we want the receiving process to take exclusive ownership of the
/// descriptor, then we need to release it here.
pub const DOOR_RELEASE: door_attr_t = 0x40000; // Passed references are also released.

/// Deliver an unref notification with door
pub const DOOR_UNREF: door_attr_t = 0x01;

/// Use a private pool of server threads
pub const DOOR_PRIVATE: door_attr_t = 0x02;

/// Deliver unref notification more than once
pub const DOOR_UNREF_MULTI: door_attr_t = 0x10;

/// Prohibit clients from sending file / socket / door descriptors
pub const DOOR_REFUSE_DESC: door_attr_t = 0x40;

/// No server thread cancel on client abort
pub const DOOR_NO_CANCEL: door_attr_t = 0x80;

/// No thread create callbacks on depletion
pub const DOOR_NO_DEPLETION_CB: door_attr_t = 0x100;

/// Descriptor is local to current process
pub const DOOR_LOCAL: door_attr_t = 0x04;

/// Door has been revoked
pub const DOOR_REVOKED: door_attr_t = 0x08;

/// Door is currently unreferenced
pub const DOOR_IS_UNREF: door_attr_t = 0x20;

/// Door has a private thread creation func
pub const DOOR_PRIVCREATE: door_attr_t = 0x200;

/// Door has a private thread creation func
pub const DOOR_DEPLETION_CB: door_attr_t = 0x400;

/// `d_data` component of [`door_desc_t`]
///
/// This is not a real doors data structure *per se*, but rather the `d_data`
/// component of the [`door_desc_t`] type. It is defined in [doors.h][1]. C
/// allows for nested type definitions, while Rust does not, so we have to
/// define each component as a separate entity.
///
/// [1]: https://github.com/illumos/illumos-gate/blob/master/usr/src/uts/common/sys/door.h#L122
#[repr(C)]
pub union door_desc_t__d_data {
    pub d_desc: door_desc_t__d_data__d_desc,
    d_resv: [libc::c_int; 5], /* Reserved by illumos for some undocumented reason */
}

/// `d_desc` component of [`door_desc_t`]
///
/// This is the `d_desc` component of the [`door_desc_t__d_data`] union of the
/// [`door_desc_t`] structure. See its original definition in [doors.h][1]. This
/// type is never created on its own, only in conjunction with creating a new
/// instance of [`door_desc_t`].
///
/// [1]: https://github.com/illumos/illumos-gate/blob/master/usr/src/uts/common/sys/door.h#L122
#[derive(Debug, Copy, Clone)]
#[repr(C, packed)]
pub struct door_desc_t__d_data__d_desc {
    pub d_descriptor: libc::c_int,
    pub d_id: door_id_t,
}

/// Opaque Door ID
///
/// Some kind of door identifier. The doors API handles this for us, we don't
/// really need to worry about it. Or at least, if I should be worried about it,
/// I'm in a lot of trouble.
pub type door_id_t = libc::c_ulonglong;

/// Door Pointer Type
///
/// Used for cookies and door identifiers.
pub type door_ptr_t = libc::c_ulonglong;

/// Structure used to return info form door_info
#[derive(Default, Clone, Copy, Debug, PartialEq)]
#[repr(C, packed)]
pub struct door_info_t {
    /// Server process
    pub di_target: libc::pid_t,

    /// Server procedure
    pub di_proc: door_ptr_t,

    /// Data cookie
    pub di_data: door_ptr_t,

    /// Attributes associated with door
    pub di_attributes: door_attr_t,

    /// Unique number
    pub di_uniquifier: door_id_t,

    /// Future use
    pub di_resv: [libc::c_int; 4],
}
