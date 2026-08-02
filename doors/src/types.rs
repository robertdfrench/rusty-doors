// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Internal aliases for the raw types.
//!
//! Short names for the raw `doors-sys` types, so the rest of the
//! crate does not spell `doors_sys::` at every use.
//!
//! Nothing here is public: `GOALS.md` §12.10 forbids a `door_desc_t`
//! union from being visible above `doors-sys`, and §11.3 is settled
//! against re-exporting the raw layer.

pub(crate) use doors_sys::{door_arg_t, door_attr_t, door_desc_t, door_info_t};

pub(crate) type ServerProcedure = doors_sys::door_server_procedure_t;
/// The `void (*)(door_info_t *)` installed by `door_server_create`.
pub(crate) type ServerThreadFunc = doors_sys::door_server_func_t;
