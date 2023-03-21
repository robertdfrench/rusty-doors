/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Copyright 2021 Robert D. French
 */
//! Unsafe Declarations for the illumos Doors API
//!
//! This module merely re-exports the subset of the illumos doors api that we need for this
//! project. It makes no attempt at safety or ergonomics.
//!
//! Check out [revolving-doors] for an introduction to doors.
//!
//! [revolving-doors]: https://github.com/robertdfrench/revolving-door#revolving-doors

#![allow(non_camel_case_types)]
use libc;

/// Signature for a Door Server Procedure
///
/// All "Server Procedures" (functions which respond to `door_call` requests) must use this type
/// signature. Because `portunusd` neither shares descriptors with applications nor makes use of the
/// `cookie` field, we can consider only:
///
/// * `argp`
/// * `arg_size`
///
/// which together specify an array of bytes.  See [`DOOR_CREATE(3C)`] for examples and further
/// detail.
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
    /// [door_server_procedure_t][1]. Portunus does not currently use the `cookie` argument. Since
    /// it will not send any file descriptors, applications are free to set `attributes` to
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
    /// Assuming `d` is a descriptor for a door which points to a function in another process, this
    /// function can use an instance of [door_arg_t] to send data to and receive data from the
    /// function described by `d`.
    ///
    /// See [`DOOR_CALL(3C)`] for more details.
    ///
    /// [`DOOR_CALL(3C)`]: https://illumos.org/man/3c/door_call
    pub fn door_call(d: libc::c_int, params: *const door_arg_t) -> libc::c_int;

    /// The inverse of `door_call` - return data and control to the calling process.
    ///
    /// Use this at the end of `server_procedure` in lieu of the traditional `return` statement to
    /// transfer control back to the process which originally issued `door_call`. Like
    /// [`EXECVE(2)`], this function is terminal from the perspective of the code which calls it.
    ///
    /// See [`DOOR_RETURN(3C)`].
    ///
    /// # Warning
    ///
    /// It is [not yet clear][1] whether Rust structures are properly cleaned up upon
    /// `door_return`. Further, because threads (and thus their state) are re-used between
    /// requests, it is vitally important that any code calling `door_return` is able to purge
    /// sensitive stack data in order to hamper an attacker's ability to exfiltrate the data of
    /// other users.
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
}

/// Arguments for, and Return Values from, a Door invocation.
///
/// This is your daily driver, right here. `data_ptr` and `data_size` represent the bytes you want
/// to send to the server. `rbuf` and `rsize` represent a space you've set aside to store bytes
/// that come back from the server; after [`DOOR_CALL(3C)`] completes, `data_ptr` and `data_size`
/// will bue updated to point inside this space. `desc_ptr` and `desc_num` are for passing any file
/// / socket / door descriptors you'd like the server to be able to access. It is described in more
/// detail below.
///
/// See [`DOOR_CALL(3C)`] for more details.
///
/// [`DOOR_CALL(3C)`]: https://illumos.org/man/3c/door_call
#[derive(Debug)]
#[repr(C)]
pub struct door_arg_t {
    pub data_ptr: *const libc::c_char,
    /// Request data from the network to the Door Application
    ///
    /// Becomes the response data after door_call completes.
    pub data_size: libc::size_t,

    pub desc_ptr: *const door_desc_t,
    /// Array of Descriptors -- unused by Portunus
    pub desc_num: libc::c_uint,

    pub rbuf: *const libc::c_char,
    /// Response data from the Door Application to the network
    pub rsize: libc::size_t,
}

/// Descriptor structure for `door_arg_t`
///
/// For our purposes, this data structure and its constituent parts are mostly opaque *except* that
/// it holds any file / socket / door descriptors which we would like to pass between processes.
/// Rust does not support nested type declaration like C does, so we define each component
/// separately. See [doors.h][1] for the original (nested) definition of this type and
/// [revolving-doors][2] for a visual guide.
///
/// [1]: https://github.com/illumos/illumos-gate/blob/master/usr/src/uts/common/sys/door.h#L122
/// [2]: https://github.com/robertdfrench/revolving-door/tree/master/A0_result_parameters
#[repr(C)]
pub struct door_desc_t {
    pub d_attributes: door_attr_t,
    pub d_data: door_desc_t__d_data,
}

/// Door config options
///
/// Specified in the "Description" section of [`DOOR_CREATE(3C)`]. The only option needed by
/// Portunus Applications is [DOOR_REFUSE_DESC](constant.DOOR_REFUSE_DESC.html).
///
/// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3c/door_create#DESCRIPTION
pub type door_attr_t = libc::c_uint;

/// Prohibit clients from sending file / socket / door descriptors
///
/// Specified in the "Description" section of [`DOOR_CREATE(3C)`]. This flag tells the illumos
/// kernel that we do not want door clients (in this case, the `portunusd` server) to be able to
/// forward their file, socket, or door descriptors to us. *This may change in a future version of
/// the [DPA][1].*
///
/// [1]: https://github.com/robertdfrench/portunusd/blob/trunk/etc/DPA.md
/// [`DOOR_CREATE(3C)`]: https://illumos.org/man/3c/door_create#DESCRIPTION
pub const DOOR_REFUSE_DESC: door_attr_t = 0x40; // Disable file descriptor passing.
pub const DOOR_DESCRIPTOR: door_attr_t = 0x10000; // A file descriptor is being passed.
pub const DOOR_RELEASE: door_attr_t = 0x40000; // Passed references are also released.

/// `d_data` component of `door_desc_t`
///
/// This is not a real doors data structure *per se*, but rather the `d_data` component of the
/// `door_desc_t` type. It is defined in [doors.h][1]. C allows for nested type definitions, while
/// Rust does not, so we have to define each component as a separate entity.
///
/// [1]: https://github.com/illumos/illumos-gate/blob/master/usr/src/uts/common/sys/door.h#L122
#[repr(C)]
pub union door_desc_t__d_data {
    pub d_desc: door_desc_t__d_data__d_desc,
    d_resv: [libc::c_int; 5], /* Reserved by illumos for some undocumented reason */
}

/// `d_desc` component of `door_desc_t`
///
/// This is the `d_desc` component of the `d_data` union of the `door_desct_t` structure. See its
/// original definition in [doors.h][1].
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
/// Some kind of door identifier. The doors API handles this for us, we don't really need to worry
/// about it. Or at least, if I should be worried about it, I'm in a lot of trouble.
pub type door_id_t = libc::c_ulonglong;

#[cfg(test)]
mod tests {
    // See /src/illumos/mod.rs for tests. The doors_h and stropts_h modules rely on each other for
    // complete functionality, so it's easier to test them together. The only reason they are
    // defined separately is to mimic how they are defined in illumos' libc headers.
}
