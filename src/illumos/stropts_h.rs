/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Copyright 2021 Robert D. French
 */

//! Unsafe Declarations for the illumos STREAMS API
//!
//! This module merely re-exports the subset of the illumos STREAMS api that we need for this
//! project. It makes no attempt at safety or ergonomics.
//!
//! While STREAMS are not strictly relevant to this project, some of their features are overloaded
//! to work with doors. Those are the bits we redefine here.

extern "C" {
    /// Makes a door descriptor visible on the filesystem.
    ///
    /// Just as network sockets must be created (as descriptors) and *then*
    /// attached to an IP Address + Port Number by calling [`BIND(3SOCKET)`],
    /// doors are created (as descriptors) and *then* attached to a path on the
    /// filesystem by calling [`FATTACH(3C)`].
    ///
    /// See [`FATTACH(3C)`] for more details.
    ///
    /// ## Example
    ///
    /// In this example we create a simple server procedure and attach it to a
    /// known location on the filesystem.
    ///
    /// ```rust
    /// use doors::illumos::door_h::{door_create, door_desc_t, door_return};
    /// use doors::illumos::errno_h::errno;
    /// use doors::illumos::stropts_h::fattach;
    /// use libc::{c_char, c_uint, c_void, size_t};
    /// use std::ffi::CString;
    /// use std::ptr::null;
    ///
    /// // Define a server procedure here so we can attach it to the filesystem
    /// // below.
    /// extern "C" fn hello(
    ///     cookie: *const c_void,
    ///     argp: *const c_char,
    ///     arg_size: size_t,
    ///     dp: *const door_desc_t,
    ///     n_desc: c_uint,
    /// ) {
    ///     println!("Hello, world!");
    ///     unsafe { door_return(null(), 0, null(), 0) };
    /// }
    ///
    /// // Create a door descriptor and a CString with a filesystem path.
    /// let descriptor = unsafe { door_create(hello, null(), 0) };
    /// let path = CString::new("/tmp/hello_world.door").unwrap();
    ///
    /// // Create a new, empty file, which must already exist before calling
    /// // fattach.
    /// std::fs::remove_file("/tmp/hello_world.door");
    /// std::fs::File::create("/tmp/hello_world.door").unwrap();
    ///
    /// // Try to attach the door path to the filesystem using fattach.
    /// let success = unsafe { fattach(descriptor, path.as_c_str().as_ptr()) };
    /// assert_eq!(success, 0);
    /// ```
    ///
    /// [`BIND(3SOCKET)`]: https://illumos.org/man/3socket/bind
    /// [`FATTACH(3C)`]: https://illumos.org/man/3c/fattach
    pub fn fattach(
        fildes: libc::c_int,
        path: *const libc::c_char,
    ) -> libc::c_int;

    /// Withdraw a door descriptor from the filesystem.
    ///
    /// After the door is detached from the filesystem, no new processes will be
    /// able to acquire a descriptor by means of [`OPEN(2)`]. Processes which
    /// already have access to the door will still be able to invoke it via
    /// [`DOOR_CALL(3C)`], and even forward the descriptor to other processes
    /// via other socket or door connections. So, we can say this call stops new
    /// clients from connecting to a door server unless an existing client
    /// shares its descriptor.
    ///
    /// See [`FDETACH(3C)`] for more details.
    ///
    /// # Example
    ///
    /// ```rust
    /// use doors::illumos::door_h::{door_create, door_desc_t, door_return};
    /// use doors::illumos::errno_h::errno;
    /// use doors::illumos::stropts_h::{fattach, fdetach};
    /// use libc::{c_char, c_uint, c_void, size_t};
    /// use std::ffi::CString;
    /// use std::ptr::null;
    ///
    /// // Define a server procedure here so we can attach it to the filesystem
    /// // below.
    /// extern "C" fn hello(
    ///     cookie: *const c_void,
    ///     argp: *const c_char,
    ///     arg_size: size_t,
    ///     dp: *const door_desc_t,
    ///     n_desc: c_uint,
    /// ) {
    ///     println!("Hello, world!");
    ///     unsafe { door_return(null(), 0, null(), 0) };
    /// }
    ///
    /// // Create a door descriptor and a CString with a filesystem path.
    /// let descriptor = unsafe { door_create(hello, null(), 0) };
    /// let path = CString::new("/tmp/hello_world2.door").unwrap();
    ///
    /// // Create a new, empty file, which must already exist before calling
    /// // fattach.
    /// std::fs::remove_file("/tmp/hello_world2.door");
    /// std::fs::File::create("/tmp/hello_world2.door").unwrap();
    ///
    /// // Try to attach the door path to the filesystem using fattach.
    /// unsafe { fattach(descriptor, path.as_c_str().as_ptr()) };
    ///
    /// // Try to detach the door from the filesystem using fdetach.
    /// let success = unsafe { fdetach(path.as_c_str().as_ptr()) };
    /// assert_eq!(success, 0);
    /// ```
    ///
    /// [`DOOR_CALL(3C)`]: https://illumos.org/man/3c/door_call
    /// [`OPEN(2)`]: https://illumos.org/man/2/open
    /// [`FDETACH(3C)`]: https://illumos.org/man/3C/fdetach
    pub fn fdetach(path: *const libc::c_char) -> libc::c_int;
}
