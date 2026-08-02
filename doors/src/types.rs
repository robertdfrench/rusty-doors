// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Internal aliases for the raw types.
//!
//! Short names for the raw `doors-sys` types, so the rest of the
//! crate does not spell `doors_sys::` at every use.
//!
//! Nothing here is public. A `door_desc_t` is a C union, and a union
//! must not be visible above `doors-sys`: reading the wrong arm of it
//! is unsafe, so it does not belong in a safe API. The raw layer is
//! not re-exported either. Code that wants it depends on `doors-sys`
//! directly.

pub(crate) use doors_sys::{door_arg_t, door_attr_t, door_desc_t, door_info_t};

pub(crate) type ServerProcedure = doors_sys::door_server_procedure_t;
/// The `void (*)(door_info_t *)` installed by `door_server_create`.
pub(crate) type ServerThreadFunc = doors_sys::door_server_func_t;
