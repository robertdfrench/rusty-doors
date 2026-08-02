// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Every `#[door(...)]` shape and option, compiled.
//!
//! `macro_server.rs` calls real doors. This file never opens one. It
//! exists so that the code `#[doors::server]` writes is type-checked
//! for every shape and option, whether or not a test happens to call
//! that shape (`GOALS.md` §9.1).
//!
//! The unit tests inside `door-macros` read the tokens the macro
//! produces. Only a test like this one can say whether those tokens
//! mean anything, because `door-macros` cannot depend on `doors` —
//! that would be a dependency cycle.

use doors::server::ReplyBuf;
use doors::{Descriptors, NoDescriptors, Request};
use std::ffi::{c_char, c_uint, c_void};

/// One state type carrying one door of every shape.
struct Multi;

#[doors::server]
impl Multi {
    /// The default shape, spelled by leaving the shape out.
    #[door]
    fn plain(
        &self,
        req: Request<'_, Descriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        Ok(req.data().to_vec())
    }

    /// The same shape, named, with a request limit.
    #[door(procedure, refuse_desc, request_size = ..=8192)]
    fn sized(
        &self,
        req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        Ok(req.data().to_vec())
    }

    #[door(reply_buf, refuse_desc, max_descriptors = 0)]
    fn buffered(
        &self,
        req: Request<'_, NoDescriptors>,
        out: &mut ReplyBuf,
    ) -> Result<(), std::io::Error> {
        out.write_bytes(req.data()).map_err(std::io::Error::other)
    }

    /// Only exists with the `rpc` feature, and so does the
    /// `build_doubled` the macro writes for it.
    #[cfg(feature = "rpc")]
    #[door(rpc, refuse_desc)]
    fn doubled(&self, req: u32) -> Result<u64, std::io::Error> {
        Ok(u64::from(req) * 2)
    }

    #[door(unref, private, request_size = 0..=64)]
    fn watched(
        &self,
        req: Request<'_, Descriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        Ok(req.data().to_vec())
    }

    /// The C server procedure, passed straight through. No trampoline
    /// is written for it.
    #[door(raw)]
    extern "C" fn by_hand(
        _cookie: *mut c_void,
        _argp: *mut c_char,
        _arg_size: usize,
        _dp: *mut doors_sys::door_desc_t,
        _n_desc: c_uint,
    ) {
    }

    /// Required by `#[door(unref)]` on `watched`.
    fn on_unreferenced(&self) {}
}

/// Every door has a constructor, and each one returns the same type.
///
/// The function is never called: building a door needs a live kernel.
/// Type-checking it is the whole point.
#[test]
fn every_door_has_a_constructor() {
    #[allow(dead_code)]
    fn constructors() -> Result<(), doors::Error> {
        let _: doors::Door<Multi> =
            doors::Door::builder(Multi).build_plain()?;
        let _ = doors::Door::builder(Multi).build_sized()?;
        let _ = doors::Door::builder(Multi).build_buffered()?;
        #[cfg(feature = "rpc")]
        let _ = doors::Door::builder(Multi).build_doubled()?;
        let _ = doors::Door::builder(Multi).build_watched()?;
        let _ = doors::Door::builder(Multi).build_by_hand()?;
        Ok(())
    }
}

/// A limit set in `#[door(...)]` and again on the builder is refused,
/// rather than one of the two being picked in silence
/// (`GOALS.md` §3.6).
///
/// No door is opened here: the check happens before the door is
/// created, so the test never needs the kernel.
#[test]
fn a_limit_set_twice_is_refused() {
    let err = doors::Door::builder(Multi)
        .request_size(0..=8)
        .build_sized()
        .expect_err("a request size in two places must be refused");
    assert!(
        matches!(
            err,
            doors::Error::OptionSetTwice {
                option: "request_size"
            }
        ),
        "got {err:?}"
    );

    let err = doors::Door::builder(Multi)
        .max_descriptors(3)
        .build_buffered()
        .expect_err("a descriptor limit in two places must be refused");
    assert!(
        matches!(
            err,
            doors::Error::OptionSetTwice {
                option: "max_descriptors"
            }
        ),
        "got {err:?}"
    );
}

// A builder call that clashes with nothing is not tested here. It
// would have to reach `build()`, which needs a live kernel, so that
// case belongs with the other VM tests in `macro_server.rs`.
