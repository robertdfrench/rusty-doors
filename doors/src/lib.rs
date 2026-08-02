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

pub use client::{Client, DoorParams, Reply, Untagged};
pub use descriptor::{
    DescAttributes, DescriptorPolicy, Descriptors, DoorId, NoDescriptors,
    ReceivedFd, SentFd,
};
pub use error::{
    CallError, Error, ErrorReply, ReplyTooBig, RevokeError, ServerFault,
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
/// See the [crate docs](crate) for an example, and `GOALS.md` §3 for
/// the full list of `#[door(...)]` options.
pub use door_macros::server;
