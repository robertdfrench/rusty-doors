// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Constants from `<sys/door.h>`, `<thread.h>` and `<pthread.h>`.
//!
//! Every value here was read out of the headers on an OmniOS
//! `r151058` amd64 host; see `tests/` for the program that printed
//! them. These are plain constants, so they are visible on every
//! platform even though the functions in [`crate`] are not.

use crate::types::door_attr_t;
use core::ffi::{c_int, c_long, c_void};

// ---------------------------------------------------------------------
// Create flags -- door_create(3C) / door_xcreate(3C)
// ---------------------------------------------------------------------

/// Deliver an unref notification with this door.
pub const DOOR_UNREF: door_attr_t = 0x01;

/// Use a private pool of server threads.
pub const DOOR_PRIVATE: door_attr_t = 0x02;

/// Deliver unref notification more than once.
pub const DOOR_UNREF_MULTI: door_attr_t = 0x10;

/// Do not accept descriptors from callers.
pub const DOOR_REFUSE_DESC: door_attr_t = 0x40;

/// Do not cancel the server thread when the client aborts.
pub const DOOR_NO_CANCEL: door_attr_t = 0x80;

/// No thread-create callbacks on depletion.
pub const DOOR_NO_DEPLETION_CB: door_attr_t = 0x100;

/// Door has a private thread creation function.
pub const DOOR_PRIVCREATE: door_attr_t = 0x200;

// ---------------------------------------------------------------------
// Info flags -- returned by door_info(3C)
// ---------------------------------------------------------------------

/// Descriptor is local to the current process.
pub const DOOR_LOCAL: door_attr_t = 0x04;

/// Door has been revoked.
pub const DOOR_REVOKED: door_attr_t = 0x08;

/// Door is currently unreferenced.
pub const DOOR_IS_UNREF: door_attr_t = 0x20;

/// Set only during depletion callbacks.
pub const DOOR_DEPLETION_CB: door_attr_t = 0x400;

// ---------------------------------------------------------------------
// Descriptor attributes -- door_desc_t.d_attributes
// ---------------------------------------------------------------------

/// A file descriptor is being passed.
pub const DOOR_DESCRIPTOR: door_attr_t = 0x10000;

/// Passed references are also released.
pub const DOOR_RELEASE: door_attr_t = 0x40000;

// ---------------------------------------------------------------------
// Sentinels
// ---------------------------------------------------------------------

/// An invalid door descriptor.
pub const DOOR_INVAL: c_int = -1;

/// Descriptor meaning "the current thread's binding" for
/// [`door_info`](crate::door_info).
pub const DOOR_QUERY: c_int = -2;

/// The `argp` value a server procedure sees for an unreferenced
/// invocation. `<sys/door.h>` spells this `((void *)1)`.
///
/// This is a bare address, not a pointer to an object. Nothing may
/// ever read through it; the code that sees it compares it as an
/// integer. `without_provenance` says exactly that: an address that
/// belongs to no allocation.
pub const DOOR_UNREF_DATA: *const c_void = core::ptr::without_provenance(1);

// ---------------------------------------------------------------------
// Parameters -- door_getparam(3C) / door_setparam(3C)
// ---------------------------------------------------------------------

/// Max number of request descriptors.
pub const DOOR_PARAM_DESC_MAX: c_int = 1;

/// Max bytes of request data.
pub const DOOR_PARAM_DATA_MAX: c_int = 2;

/// Min bytes of request data.
pub const DOOR_PARAM_DATA_MIN: c_int = 3;

// ---------------------------------------------------------------------
// Masks
// ---------------------------------------------------------------------

/// Every flag [`door_create`](crate::door_create) accepts.
pub const DOOR_CREATE_MASK: door_attr_t = DOOR_UNREF
    | DOOR_PRIVATE
    | DOOR_UNREF_MULTI
    | DOOR_REFUSE_DESC
    | DOOR_NO_CANCEL
    | DOOR_NO_DEPLETION_CB
    | DOOR_PRIVCREATE;

/// Every attribute [`door_info`](crate::door_info) may report.
pub const DOOR_ATTR_MASK: door_attr_t =
    DOOR_CREATE_MASK | DOOR_LOCAL | DOOR_REVOKED | DOOR_IS_UNREF;

// The header spells these out numerically. Assert our derivations
// agree, so a typo above cannot pass unnoticed.
const _: () = assert!(DOOR_CREATE_MASK == 0x3d3);
const _: () = assert!(DOOR_ATTR_MASK == 0x3ff);

// ---------------------------------------------------------------------
// <thread.h> flags
// ---------------------------------------------------------------------

/// Bind the new thread to an LWP. Same value as `PTHREAD_SCOPE_SYSTEM`.
pub const THR_BOUND: c_long = 0x00000001;

/// Give the new thread a new LWP.
pub const THR_NEW_LWP: c_long = 0x00000002;

/// Same value as `PTHREAD_CREATE_DETACHED`.
pub const THR_DETACHED: c_long = 0x00000040;

/// Create the thread suspended.
pub const THR_SUSPENDED: c_long = 0x00000080;

/// A daemon thread does not hold the process open at exit.
pub const THR_DAEMON: c_long = 0x00000100;

// ---------------------------------------------------------------------
// <pthread.h>
// ---------------------------------------------------------------------

/// Argument to `pthread_setcancelstate`. Every door server thread sets
/// this; see `GOALS.md` §5.5.
pub const PTHREAD_CANCEL_DISABLE: c_int = 0x01;

/// The complement of [`PTHREAD_CANCEL_DISABLE`], for completeness.
pub const PTHREAD_CANCEL_ENABLE: c_int = 0x00;
