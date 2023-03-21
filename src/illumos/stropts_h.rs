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
    /// Just like sockets must be created (as descriptors) and *then* attached to an IP Address +
    /// Port Number by calling [`BIND(3SOCKET)`], doors are created (as descriptors) and *then*
    /// attached to a path on the filesystem by calling [`FATTACH(3C)`].
    ///
    /// [`BIND(3SOCKET)`]: https://illumos.org/man/3socket/bind
    /// [`FATTACH(3C)`]: https://illumos.org/man/3c/fattach
    pub fn fattach(
        fildes: libc::c_int,
        path: *const libc::c_char,
    ) -> libc::c_int;

    /// Withdraw a door descriptor from the filesystem.
    ///
    /// After the door is detached from the filesystem, no new processes will be able to acquire
    /// a descriptor by means of [`OPEN(2)`]. Processes which already have access to the door will
    /// still be able to invoke it via [`DOOR_CALL(3C)`], and even forward the descriptor to other
    /// processes via other socket or door connections. So, we can say this call stops new clients
    /// from connecting to a door server unless an existing client shares its descriptor.
    ///
    /// [`DOOR_CALL(3C)`]: https://illumos.org/man/3c/door_call
    /// [`OPEN(2)`]: https://illumos.org/man/2/open
    pub fn fdetach(path: *const libc::c_char) -> libc::c_int;
}

#[repr(C)]
pub struct strrecvfd {
    pub fd: libc::c_int,
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
    fill: [libc::c_char; 8],
}

#[cfg(test)]
mod tests {
    // See /src/illumos/mod.rs for tests. The doors_h and stropts_h modules rely on each other for
    // complete functionality, so it's easier to test them together. The only reason they are
    // defined separately is to mimic how they are defined in illumos' libc headers.
}
