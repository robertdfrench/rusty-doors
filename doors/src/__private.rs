// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Support code for `#[doors::server]`. Not a public API.
//!
//! Generated code refers only to paths inside this module. That keeps
//! two promises at once: the macro never depends on a name a user
//! could shadow, and the real public API stays free to change without
//! breaking code someone generated last year.
//!
//! Nothing here is covered by semantic versioning. Do not use it
//! directly.

pub use crate::descriptor::{Descriptors, NoDescriptors};
pub use crate::error::{ErrorReply, ServerFault, StatusTag};
pub use crate::server::builder::DoorBuilder;
pub use crate::server::reply_buf::ReplyBuf;
pub use crate::server::request::Request;
pub use crate::server::trampoline::{
    run, Outcome, ReplyProtocol, MAX_REPLY_DESCRIPTORS,
};
pub use crate::server::Door;
pub use crate::Error;

pub use std::ffi::{c_char, c_uint, c_void};

/// Refuse a door whose limits were set in two places.
///
/// `#[door(request_size = ..=8192)]` and
/// [`DoorBuilder::request_size`] set the same kernel parameter, and so
/// do `#[door(max_descriptors = 4)]` and
/// [`DoorBuilder::max_descriptors`]. A generated `build_foo()` calls
/// this before it applies its own.
///
/// # Why the check is here and not in the compiler
///
/// The macro sees one `impl` block. The builder chain is written
/// somewhere else, and `request_size()` hands back the same type it
/// took, so the call leaves no trace in the type for `build_foo()` to
/// look at. Making the builder change type on every call would make
/// every ordinary chain harder to read for the sake of one rare
/// mistake. So the builder records the call in a flag instead, and
/// this function reads it.
///
/// Silently picking one of the two values is the one thing we must not
/// do: the door would then be created with limits nobody asked for.
pub fn check_builder_conflicts<S>(
    builder: &DoorBuilder<S>,
    request_size: bool,
    max_descriptors: bool,
) -> Result<(), Error>
where
    S: Send + Sync + 'static,
{
    if request_size && builder.explicit_request_size {
        return Err(Error::OptionSetTwice {
            option: "request_size",
        });
    }
    if max_descriptors && builder.explicit_max_descriptors {
        return Err(Error::OptionSetTwice {
            option: "max_descriptors",
        });
    }
    Ok(())
}

/// Which reply framing this builder was asked for.
///
/// # Why generated code has to ask
///
/// The kernel calls a server procedure with five arguments of its
/// own choosing, and there is no sixth for us. So the framing cannot
/// be handed to the entry point at run time; it has to be baked into
/// which entry point is registered.
///
/// A generated `build_foo()` therefore emits two entry points, reads
/// this, and registers the matching one. That way
/// [`DoorBuilder::untagged`] and `#[door(untagged)]` both work, and
/// neither can be quietly ignored.
pub fn reply_protocol<S>(builder: &DoorBuilder<S>) -> ReplyProtocol
where
    S: Send + Sync + 'static,
{
    builder.reply_protocol()
}

/// The raw `door_desc_t`, for the generated `extern "C"` signature
/// only. Generated code never reads a field of it: the trampoline
/// does that, because the struct is packed and a misplaced reference
/// would be undefined behaviour.
pub use doors_sys::door_desc_t;

/// Attribute bits, so generated code does not have to name
/// `doors-sys`.
pub mod attrs {
    pub use doors_sys::{
        DOOR_NO_CANCEL, DOOR_PRIVATE, DOOR_REFUSE_DESC, DOOR_UNREF,
        DOOR_UNREF_MULTI,
    };
}

/// Serialisation for the `#[door(rpc)]` shape.
///
/// Behind the `rpc` feature, so the base crate does not pull in
/// `serde` for people who do not use it. `door-macros` never names
/// `serde` or `postcard`; it emits calls to these two functions
/// instead.
#[cfg(feature = "rpc")]
pub mod rpc {
    /// Decode a request body.
    pub fn decode_request<T>(bytes: &[u8]) -> Result<T, postcard::Error>
    where
        T: serde::de::DeserializeOwned,
    {
        postcard::from_bytes(bytes)
    }

    /// Encode a reply body.
    pub fn encode_reply<T>(value: &T) -> Result<Vec<u8>, postcard::Error>
    where
        T: serde::Serialize,
    {
        postcard::to_allocvec(value)
    }
}
