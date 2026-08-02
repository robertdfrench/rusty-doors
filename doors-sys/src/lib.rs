// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Raw FFI bindings for the [illumos Doors API][1].
//!
//! This crate is the bottom of the workspace. It is the C surface and
//! nothing else: no safety, no ergonomics, no allocation. If you want
//! an API that upholds invariants for you, use [`doors`][2] instead.
//!
//! # What is here
//!
//! - Every function from `<door.h>`, plus `fattach`/`fdetach` from
//!   `<stropts.h>` and the `<thread.h>` and `<pthread.h>` calls the
//!   safe layer needs.
//! - Every type and constant from `<sys/door.h>`, laid out to match
//!   the C ABI exactly (see [`layout`]).
//!
//! # Two departures from "no wrappers"
//!
//! Everything is re-exported exactly as C declares it, with two
//! exceptions:
//!
//! - [`door_return`] returns [`Errno`] rather than `c_int`, because it
//!   only ever returns on failure. Zero is not a possible result, and
//!   the type says so.
//! - [`errno`] exists at all, because illumos reaches errno through a
//!   per-thread accessor rather than a global.
//!
//! # illumos only
//!
//! Doors are an illumos facility. This crate is for illumos and
//! nothing else. It does not build on any other system, and it does
//! not try to. There are no stubs, no fallbacks and no `target_os`
//! gates: every declaration here names a symbol that only libc on
//! illumos provides.
//!
//! Build and test it on illumos.
//!
//! [1]: https://illumos.org/man/3C/door_create
//! [2]: https://docs.rs/doors

#![no_std]
#![allow(non_camel_case_types)]
#![deny(missing_docs)]

mod consts;
mod ffi;
pub mod layout;
mod types;

pub use consts::*;
pub use ffi::*;
pub use types::*;
