// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A Rust interface for [illumos Doors][1].
//!
//! Doors are a fast way for two processes on the same machine to talk.
//! A client calls a door; the kernel runs a procedure in the server
//! process on the calling thread's behalf and comes straight back,
//! without ever giving up the CPU. When latency matters they beat
//! pipes and UNIX domain sockets.
//!
//! # illumos only
//!
//! Doors are an illumos facility. This crate is for illumos and
//! nothing else. It does not build on any other system, and it does
//! not try to: there are no stubs and no fallbacks. Build it, test
//! it and run it on illumos.
//!
//! # What this crate is for
//!
//! The C interface is fast but hard to use correctly. The problems are
//! not mostly about memory safety; they are about things the type
//! system could enforce and C cannot:
//!
//! - Nothing stops a client sending descriptors to a door that refuses
//!   them. Here, a [`Client<NoDescriptors>`] has no method that sends
//!   one, so the mistake is a compile error.
//! - A client that receives a large reply gets a fresh memory mapping
//!   and must remember to unmap it. Here, [`Reply`] unmaps on `Drop`,
//!   always.
//! - `door_return(3C)` usually does not return, so no destructor on
//!   the server thread ever runs. Here, the generated trampoline drops
//!   everything before calling it. See [`server::trampoline`].
//! - A door does not survive `fork`, but the descriptor does, and a
//!   careless child tears down the parent's door. Here, [`fork`]
//!   handles that, with a `pthread_atfork` backstop for forks that go
//!   around it.
//!
//! # Calling a door
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use doors::Client;
//!
//! let client = Client::open("/var/run/my_door")?;
//! let reply = client.call(b"ping")?;
//! println!("{}", String::from_utf8_lossy(reply.data()));
//! # Ok(())
//! # }
//! ```
//!
//! # Calling a door somebody handed you
//!
//! A door can be passed from one process to another. When it arrives
//! it is a descriptor, and there is no path to open, so
//! [`Client::open`] is no use. Two ways in, for two situations:
//!
//! ```no_run
//! use doors::{Client, MaybeDoor, Reply};
//! use std::os::fd::OwnedFd;
//!
//! type Fallible<T> = Result<T, Box<dyn std::error::Error>>;
//!
//! // It came out of a door call. `from_received` takes the descriptor
//! // in the type the kernel delivered it in, and checks it.
//! fn adopt(reply: Reply) -> Fallible<Client> {
//!     let arrived = reply.into_descriptors().pop().expect("a door");
//!     Ok(Client::from_received(arrived)?)
//! }
//!
//! // It came from somewhere else: inherited across an exec, passed
//! // over a socket, named on the command line. Nothing has said what
//! // it is, so ask. If it is not a door, the error hands the
//! // descriptor back.
//! fn adopt_unknown(fd: OwnedFd) -> Fallible<Client> {
//!     Ok(MaybeDoor::new(fd).into_client()?)
//! }
//! ```
//!
//! To call a door without taking it over at all, use
//! [`BorrowedClient`].
//!
//! # Serving a door
//!
//! ```ignore
//! struct Greeter { greeting: String }
//!
//! #[doors::server]
//! impl Greeter {
//!     #[door(refuse_desc)]
//!     fn hello(&self, req: Request<'_, NoDescriptors>)
//!         -> Result<Vec<u8>, std::io::Error>
//!     {
//!         Ok(format!("{}, {}", self.greeting,
//!                    String::from_utf8_lossy(req.data())).into_bytes())
//!     }
//! }
//!
//! let mut door = Door::builder(Greeter { greeting: "hello".into() })
//!     .thread_stack_size(256 * 1024)
//!     .build_hello()?;
//! door.attach("/var/run/my_door")?;
//! ```
//!
//! # A warning about shared libraries
//!
//! This crate registers `pthread_atfork` handlers the first time a
//! door is built, and there is no way to unregister them. If it is
//! linked into a `cdylib` that is later `dlclose`d, those handlers
//! point into unmapped memory and the next `fork` in that process
//! crashes.
//!
//! Link it into an executable. That is the ordinary case for a door
//! server anyway; a door that comes and goes with a shared library
//! would be a strange thing to build.
//!
//! [1]: https://illumos.org/man/3C/door_create
//! [`Client<NoDescriptors>`]: Client

#![deny(missing_docs)]
#![warn(clippy::undocumented_unsafe_blocks)]

mod client;
mod descriptor;
mod error;
mod registry;
mod sys;
mod types;

pub mod server;

#[doc(hidden)]
pub mod __private;

pub use client::{
    BorrowedClient, Client, DoorParams, MaybeDoor, Reply, Untagged,
};
pub use descriptor::{
    DescAttributes, DescriptorPolicy, Descriptors, DoorId, NoDescriptors,
    ReceivedFd, SentFd,
};
pub use error::{
    CallError, Error, ErrorReply, NotADoor, NotADoorReason, ReplyTooBig,
    RevokeError, ServerFault,
};
pub use registry::{fork, ForkResult};
pub use server::{
    Door, DoorBuilder, DoorInfo, ReplyBuf, ReplyProtocol, Request, UCred,
};

/// A non-zero errno.
///
/// Re-exported from `doors-sys` so callers can match on a system error
/// without depending on the raw layer directly.
pub use doors_sys::Errno;

/// Turn a Rust `impl` block into a door server.
///
/// Put `#[doors::server]` on the `impl` block. It takes no options of
/// its own. Every method that should become a door gets a
/// `#[door(...)]` of its own, and the macro writes a
/// `build_<method>()` for each one. See the [crate docs](crate) for a
/// worked example.
///
/// # Shapes
///
/// A server procedure can be written in several forms. They differ in
/// what the function takes and returns: raw bytes, a serialised type,
/// a buffer to write into, or bytes plus descriptors. That choice is
/// the method's *shape*.
///
/// There is more than one because doors are used for very different
/// jobs. Some servers just move bytes. Some want a Rust type in and a
/// Rust type out, and would rather not write the encoding themselves.
/// Some want to write straight into the reply buffer and never
/// allocate. Some have to hand a file descriptor back. One signature
/// could not serve all of those without being clumsy for every one of
/// them.
///
/// Choose a shape with at most one keyword. When none is given the
/// shape is `procedure`. Each shape generates its own
/// `build_<method>()`.
///
/// | Keyword | Signature |
/// |---|---|
/// | `procedure` | `fn(&self, Request<'_, D>) -> Result<Vec<u8>, E>` |
/// | `rpc` | `fn(&self, Req) -> Result<Resp, E>` |
/// | `reply_buf` | `fn(&self, Request<'_, D>, &mut ReplyBuf) -> Result<(), E>` |
/// | `handback` | `fn(&self, Request<'_, D>) -> Result<(Vec<u8>, Vec<OwnedFd>), E>` |
/// | `raw` | the C server procedure, passed through untouched |
///
/// `D` is [`NoDescriptors`] when `refuse_desc` is set, and
/// [`Descriptors`] otherwise. `handback` is the only shape whose reply
/// can carry descriptors.
///
/// # Flags
///
/// Any combination, alongside the shape.
///
/// | Keyword | What it does |
/// |---|---|
/// | `refuse_desc` | The door refuses descriptors, in both directions. |
/// | `unref` | Ask for an unreferenced notification. Needs an `on_unreferenced` method. |
/// | `unref_multi` | The same, but repeated. |
/// | `private` | Give this door its own pool of server threads. |
/// | `untagged` | Reply with no status byte, for a caller that does not use this crate. |
/// | `request_size = ..=8192` | The largest request accepted. |
/// | `max_descriptors = 4` | The most descriptors one call may carry. |
///
/// `DOOR_NO_CANCEL` is not an option. The builder always sets it, and
/// there is no way to clear it.
///
/// # Before you use `handback`
///
/// Three rules, because none of them fail in an obvious way:
///
/// - A reply carries at most sixteen descriptors. Any past that are
///   closed, not sent, and the call still succeeds. Return sixteen or
///   fewer.
/// - Do not add `refuse_desc`. It stops descriptors in both
///   directions, so no client could read what the door sends back. Use
///   `max_descriptors = 0` to turn away descriptors the *caller*
///   sends.
/// - The caller has to ask for them too. A client only reads
///   descriptors out of a reply if it was built with
///   [`Client::with_descriptors`].
///
/// [`NoDescriptors`]: crate::NoDescriptors
/// [`Descriptors`]: crate::Descriptors
/// [`Client::with_descriptors`]: crate::Client::with_descriptors
pub use door_macros::server;
