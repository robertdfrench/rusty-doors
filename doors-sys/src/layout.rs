// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The C ABI these bindings must reproduce, as constants.
//!
//! Every number here came from compiling this against the real headers
//! on an OmniOS `r151058` amd64 host:
//!
//! ```c
//! printf("%zu %zu\n", sizeof(door_desc_t), _Alignof(door_desc_t));
//! printf("%zu\n", offsetof(door_info_t, di_proc));
//! ```
//!
//! The `const _: () = assert!(...)` blocks below run at compile time.
//! A wrong `#[repr]` therefore fails the build instead of quietly
//! corrupting memory at run time.
//!
//! Two of these are easy to get wrong and worth stating plainly:
//! `door_info_t` is 4-aligned, which puts the 8-byte `di_proc` at
//! offset 4; and `door_desc_t` is 24 bytes rather than the 16 its
//! `d_desc` arm would suggest, because the `d_resv[5]` arm is larger.

use crate::types::*;
use core::mem::{align_of, offset_of, size_of};

// ---------------------------------------------------------------------
// Expected values, measured on illumos amd64
// ---------------------------------------------------------------------

/// `sizeof(door_desc_t)` on amd64.
pub const DOOR_DESC_T_SIZE: usize = 24;
/// `_Alignof(door_desc_t)` on amd64. 4, not 8: `#pragma pack(4)`.
pub const DOOR_DESC_T_ALIGN: usize = 4;

/// `sizeof(door_info_t)` on amd64.
pub const DOOR_INFO_T_SIZE: usize = 48;
/// `_Alignof(door_info_t)` on amd64. 4, not 8: `#pragma pack(4)`.
pub const DOOR_INFO_T_ALIGN: usize = 4;

/// `sizeof(door_arg_t)` on amd64.
pub const DOOR_ARG_T_SIZE: usize = 48;
/// `_Alignof(door_arg_t)` on amd64. Declared outside the pack guard.
pub const DOOR_ARG_T_ALIGN: usize = 8;

/// `sizeof(door_cred_t)` on amd64.
pub const DOOR_CRED_T_SIZE: usize = 36;
/// `_Alignof(door_cred_t)` on amd64.
pub const DOOR_CRED_T_ALIGN: usize = 4;

/// `sizeof(door_return_desc_t)` on amd64.
pub const DOOR_RETURN_DESC_T_SIZE: usize = 16;
/// `_Alignof(door_return_desc_t)` on amd64.
pub const DOOR_RETURN_DESC_T_ALIGN: usize = 8;

// ---------------------------------------------------------------------
// door_desc_t
// ---------------------------------------------------------------------

const _: () = assert!(size_of::<door_desc_t>() == DOOR_DESC_T_SIZE);
const _: () = assert!(align_of::<door_desc_t>() == DOOR_DESC_T_ALIGN);
const _: () = assert!(offset_of!(door_desc_t, d_attributes) == 0);
const _: () = assert!(offset_of!(door_desc_t, d_data) == 4);

// The union is 20 bytes because of d_resv, not 12 because of d_desc.
const _: () = assert!(size_of::<door_desc_data>() == 20);
const _: () = assert!(align_of::<door_desc_data>() == 4);
const _: () = assert!(size_of::<door_desc_desc>() == 12);
const _: () = assert!(align_of::<door_desc_desc>() == 4);
const _: () = assert!(offset_of!(door_desc_desc, d_descriptor) == 0);
// d_id is a u64 sitting at a 4-aligned offset. This is the whole
// reason packed(4) is required.
const _: () = assert!(offset_of!(door_desc_desc, d_id) == 4);

// ---------------------------------------------------------------------
// door_info_t
// ---------------------------------------------------------------------

const _: () = assert!(size_of::<door_info_t>() == DOOR_INFO_T_SIZE);
const _: () = assert!(align_of::<door_info_t>() == DOOR_INFO_T_ALIGN);
const _: () = assert!(offset_of!(door_info_t, di_target) == 0);
const _: () = assert!(offset_of!(door_info_t, di_proc) == 4);
const _: () = assert!(offset_of!(door_info_t, di_data) == 12);
const _: () = assert!(offset_of!(door_info_t, di_attributes) == 20);
const _: () = assert!(offset_of!(door_info_t, di_uniquifier) == 24);
const _: () = assert!(offset_of!(door_info_t, di_resv) == 32);

// ---------------------------------------------------------------------
// door_arg_t
// ---------------------------------------------------------------------

const _: () = assert!(size_of::<door_arg_t>() == DOOR_ARG_T_SIZE);
const _: () = assert!(align_of::<door_arg_t>() == DOOR_ARG_T_ALIGN);
const _: () = assert!(offset_of!(door_arg_t, data_ptr) == 0);
const _: () = assert!(offset_of!(door_arg_t, data_size) == 8);
const _: () = assert!(offset_of!(door_arg_t, desc_ptr) == 16);
const _: () = assert!(offset_of!(door_arg_t, desc_num) == 24);
const _: () = assert!(offset_of!(door_arg_t, rbuf) == 32);
const _: () = assert!(offset_of!(door_arg_t, rsize) == 40);

// ---------------------------------------------------------------------
// door_cred_t
// ---------------------------------------------------------------------

const _: () = assert!(size_of::<door_cred_t>() == DOOR_CRED_T_SIZE);
const _: () = assert!(align_of::<door_cred_t>() == DOOR_CRED_T_ALIGN);
const _: () = assert!(offset_of!(door_cred_t, dc_euid) == 0);
const _: () = assert!(offset_of!(door_cred_t, dc_egid) == 4);
const _: () = assert!(offset_of!(door_cred_t, dc_ruid) == 8);
const _: () = assert!(offset_of!(door_cred_t, dc_rgid) == 12);
const _: () = assert!(offset_of!(door_cred_t, dc_pid) == 16);
const _: () = assert!(offset_of!(door_cred_t, dc_resv) == 20);

// ---------------------------------------------------------------------
// door_return_desc_t
// ---------------------------------------------------------------------

const _: () =
    assert!(size_of::<door_return_desc_t>() == DOOR_RETURN_DESC_T_SIZE);
const _: () =
    assert!(align_of::<door_return_desc_t>() == DOOR_RETURN_DESC_T_ALIGN);
const _: () = assert!(offset_of!(door_return_desc_t, desc_ptr) == 0);
const _: () = assert!(offset_of!(door_return_desc_t, desc_num) == 8);

// ---------------------------------------------------------------------
// Scalars
// ---------------------------------------------------------------------

const _: () = assert!(size_of::<door_attr_t>() == 4);
const _: () = assert!(size_of::<door_id_t>() == 8);
const _: () = assert!(size_of::<door_ptr_t>() == 8);

/// The same assertions again as a runnable test, so `cargo test`
/// reports them by name rather than only failing the build.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn door_desc_t_matches_c() {
        assert_eq!(size_of::<door_desc_t>(), DOOR_DESC_T_SIZE);
        assert_eq!(align_of::<door_desc_t>(), DOOR_DESC_T_ALIGN);
        assert_eq!(size_of::<door_desc_data>(), 20);
        assert_eq!(offset_of!(door_desc_desc, d_id), 4);
    }

    #[test]
    fn door_info_t_matches_c() {
        assert_eq!(size_of::<door_info_t>(), DOOR_INFO_T_SIZE);
        assert_eq!(align_of::<door_info_t>(), DOOR_INFO_T_ALIGN);
        assert_eq!(offset_of!(door_info_t, di_proc), 4);
        assert_eq!(offset_of!(door_info_t, di_uniquifier), 24);
    }

    #[test]
    fn door_arg_t_matches_c() {
        assert_eq!(size_of::<door_arg_t>(), DOOR_ARG_T_SIZE);
        assert_eq!(align_of::<door_arg_t>(), DOOR_ARG_T_ALIGN);
        assert_eq!(offset_of!(door_arg_t, rsize), 40);
    }

    #[test]
    fn door_cred_t_matches_c() {
        assert_eq!(size_of::<door_cred_t>(), DOOR_CRED_T_SIZE);
        assert_eq!(align_of::<door_cred_t>(), DOOR_CRED_T_ALIGN);
    }

    #[test]
    fn masks_match_the_header() {
        assert_eq!(crate::DOOR_CREATE_MASK, 0x3d3);
        assert_eq!(crate::DOOR_ATTR_MASK, 0x3ff);
    }
}
