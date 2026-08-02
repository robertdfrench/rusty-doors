// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Every error this crate can produce.

use crate::descriptor::DoorId;
use doors_sys::Errno;
use std::fmt;
use std::os::fd::OwnedFd;

/// The status byte that leads every trampoline reply.
///
/// The server writes one of these, then the payload; the client reads
/// it back and turns tags 1 and 2 into [`CallError::Server`] and
/// [`CallError::ServerFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StatusTag {
    /// The payload is the user function's reply bytes.
    Ok = 0,
    /// The user function returned `Err(E)`; the payload is `E`'s
    /// encoding.
    UserError = 1,
    /// Infrastructure failure; the payload is a [`ServerFault`]
    /// discriminant.
    Fault = 2,
}

impl StatusTag {
    /// Read a tag off the wire. Anything we did not write is a
    /// protocol error rather than a silently accepted default.
    pub(crate) fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Ok),
            1 => Some(Self::UserError),
            2 => Some(Self::Fault),
            _ => None,
        }
    }
}

/// A failure inside the crate's own machinery, rather than in the
/// user's server procedure.
///
/// Deliberately carries no detail from the server process. A panic
/// message can hold anything the server had in scope, and shipping it
/// to whoever called the door would leak it across a trust boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServerFault {
    /// The server procedure panicked and `catch_unwind` caught it.
    Panicked,
    /// The cookie could not be resolved to live server state.
    ///
    /// Two different faults arrive here, and the client cannot tell
    /// them apart. A reply carries one status byte, so there is no
    /// room on the wire for a reason.
    ///
    /// **1. The state is gone.** The door is being revoked, or its
    /// slab entry has already been taken away. A call that was already
    /// on its way in finds nothing to run against. This is a race, and
    /// a normal one.
    ///
    /// **2. The state was asked for by the wrong type.** This is the
    /// common one, and it does not read like this error at all.
    ///
    /// State is stored under the type it was built with, and looked up
    /// by that type. `Door::builder(state)` fixes the type; a
    /// hand-written server procedure that calls
    /// `doors::__private::run::<S, ...>` has to name the same one. If
    /// the door was built with `Door::builder(Arc::new(app))` and the
    /// procedure says `run::<App, ...>`, the two do not match. Nothing
    /// ties them together, so it compiles, and then the lookup misses
    /// on **every** call, for the life of the process.
    ///
    /// The tell is that it never works, not even once. A revoke race
    /// hits one call in thousands; a wrong type hits all of them.
    ///
    /// The server prints one line to standard error the first time
    /// this happens, naming both types. Look there.
    StateUnavailable,
    /// The reply did not fit in the server's [`ReplyBuf`] and the hard
    /// cap refused to grow.
    ///
    /// [`ReplyBuf`]: crate::server::ReplyBuf
    ReplyTooBig,
}

impl ServerFault {
    pub(crate) fn as_byte(self) -> u8 {
        match self {
            Self::Panicked => 0,
            Self::StateUnavailable => 1,
            Self::ReplyTooBig => 2,
        }
    }

    pub(crate) fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Panicked),
            1 => Some(Self::StateUnavailable),
            2 => Some(Self::ReplyTooBig),
            _ => None,
        }
    }
}

impl fmt::Display for ServerFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Panicked => "the server procedure panicked",
            Self::StateUnavailable => {
                "the server could not find its state: either it was \
                 dropped, or the server procedure asked for it by the \
                 wrong type"
            }
            Self::ReplyTooBig => "the reply exceeded the server's limit",
        };
        f.write_str(s)
    }
}

impl std::error::Error for ServerFault {}

/// The reply did not fit, and the buffer is not allowed to grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyTooBig {
    /// How many bytes the reply needed.
    pub needed: usize,
    /// The cap that refused it.
    pub limit: usize,
}

impl fmt::Display for ReplyTooBig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "reply needs {} bytes but the limit is {}",
            self.needed, self.limit
        )
    }
}

impl std::error::Error for ReplyTooBig {}

/// A general failure from a doors operation.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A system call failed. Carries the errno and which call it was.
    Sys {
        /// Which C function failed.
        call: &'static str,
        /// The errno it left behind.
        errno: Errno,
    },
    /// The door was disowned by a `fork`, so this process must not act
    /// on it. A child never inherits a working door; it must create
    /// its own. See [`crate::fork`].
    Disowned,
    /// The door refuses descriptors, so
    /// [`Client::with_descriptors`](crate::Client::with_descriptors)
    /// cannot succeed.
    RefusesDescriptors,
    /// A path could not be represented as a C string, because it
    /// contains an interior NUL.
    PathHasNul,
    /// The requested server thread stack is too small for the declared
    /// request size. Request data, descriptors and `door_info_t` all
    /// land on that stack.
    StackTooSmall {
        /// The stack size asked for.
        requested: usize,
        /// The smallest stack that could work.
        needed: usize,
    },
    /// A limit was set both in `#[door(...)]` and on the builder, so
    /// there is no way to tell which one was meant. Set it in one
    /// place only.
    OptionSetTwice {
        /// The name of the option, as it is spelled in `#[door(...)]`.
        option: &'static str,
    },
}

impl Error {
    pub(crate) fn sys(call: &'static str) -> Self {
        Error::Sys {
            call,
            errno: crate::sys::last_errno(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sys { call, errno } => {
                write!(f, "{call} failed: errno {}", errno.get())
            }
            Error::Disowned => f.write_str(
                "this door belongs to a process that forked away from it",
            ),
            Error::RefusesDescriptors => {
                f.write_str("this door was created with DOOR_REFUSE_DESC")
            }
            Error::PathHasNul => {
                f.write_str("path contains an interior NUL byte")
            }
            Error::StackTooSmall { requested, needed } => write!(
                f,
                "server thread stack of {requested} bytes is too small; \
                 the declared request size needs at least {needed}"
            ),
            Error::OptionSetTwice { option } => write!(
                f,
                "`{option}` is set both in `#[door(...)]` and on the \
                 builder; remove one of them"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Sys { errno, .. } => {
                std::io::Error::from_raw_os_error(errno.get())
            }
            other => std::io::Error::other(other),
        }
    }
}

/// Why [`Door::revoke`](crate::Door::revoke) did not hand the state
/// back.
#[derive(Debug)]
#[non_exhaustive]
pub enum RevokeError {
    /// A `fork` disowned this door; the child must not revoke the
    /// parent's door.
    Disowned,
    /// `door_revoke` itself failed.
    Sys {
        /// The errno it left behind.
        errno: Errno,
    },
    /// The door was revoked and drained, but something outside the
    /// crate still holds a reference to the state, so it cannot be
    /// handed back by value.
    StateStillShared,
}

impl fmt::Display for RevokeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RevokeError::Disowned => {
                f.write_str("this door was disowned by a fork")
            }
            RevokeError::Sys { errno } => {
                write!(f, "door_revoke failed: errno {}", errno.get())
            }
            RevokeError::StateStillShared => f.write_str(
                "the door was revoked, but the state is still shared",
            ),
        }
    }
}

impl std::error::Error for RevokeError {}

/// Why a [`door_call`] did not produce a reply.
///
/// The descriptor rules here are not advisory. `door_call` consumes
/// the descriptors it was given on almost every path, so which variant
/// you get decides whether the caller still owns them.
///
/// [`door_call`]: crate::Client::call
#[derive(Debug)]
#[non_exhaustive]
pub enum CallError {
    /// `EFAULT` or `EBADF`. The kernel rejected the call before taking
    /// the descriptors, so any `Released` ones are handed back intact.
    Rejected {
        /// The descriptors the caller passed by value, returned.
        returned: Vec<OwnedFd>,
        /// The errno.
        errno: Errno,
    },
    /// Any other errno. The kernel consumed the descriptors; there is
    /// nothing to hand back.
    Consumed(Errno),
    /// The call was interrupted. **The server may already have run.**
    /// The descriptors were consumed.
    Interrupted,
    /// The reply did not fit and the caller asked for no mapping.
    ReplyTooBig {
        /// How many bytes the reply needed.
        needed: usize,
    },
    /// The server procedure returned `Err`. Only a tagged reply can
    /// say this.
    Server {
        /// The error's encoding, as the server wrote it.
        data: Vec<u8>,
    },
    /// The server panicked or could not resolve its state. Only a
    /// tagged reply can say this.
    ServerFailed(ServerFault),
    /// The server sent a reply this crate cannot parse. Either it is
    /// not a `doors` server, or the two sides disagree about the
    /// protocol.
    Protocol(&'static str),
    /// A `Client<NoDescriptors>` received descriptors anyway. They have
    /// already been closed; there is nothing for the caller to clean
    /// up.
    UnexpectedDescriptors {
        /// How many arrived.
        count: usize,
    },
}

impl CallError {
    /// The door ids of any descriptors involved, when we know them.
    /// Present so callers can log without reaching for the raw union.
    pub fn door_ids(&self) -> &[DoorId] {
        &[]
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Rejected { returned, errno } => write!(
                f,
                "door_call rejected (errno {}); {} descriptor(s) returned",
                errno.get(),
                returned.len()
            ),
            CallError::Consumed(errno) => write!(
                f,
                "door_call failed (errno {}); descriptors were consumed",
                errno.get()
            ),
            CallError::Interrupted => f.write_str(
                "door_call was interrupted; the server may already have run",
            ),
            CallError::ReplyTooBig { needed } => {
                write!(f, "reply needs {needed} bytes and would not fit")
            }
            CallError::Server { data } => {
                write!(f, "the server returned an error ({} bytes)", data.len())
            }
            CallError::ServerFailed(fault) => write!(f, "{fault}"),
            CallError::Protocol(why) => {
                write!(f, "malformed reply: {why}")
            }
            CallError::UnexpectedDescriptors { count } => write!(
                f,
                "server sent {count} descriptor(s) to a client that does \
                 not accept them; they have been closed"
            ),
        }
    }
}

impl std::error::Error for CallError {}

/// A descriptor was handed over as a door, and it was not one.
///
/// # It gives the descriptor back
///
/// This is the whole point of having its own type. The caller offered
/// a descriptor and the offer was refused, so the descriptor is still
/// theirs. Dropping it here would close a file they may still want,
/// and that would be a worse outcome than the mistake that caused the
/// refusal.
///
/// [`CallError::Rejected`] hands sent descriptors back for the same
/// reason. This follows it.
///
/// ```no_run
/// # use doors::{Client, Probably};
/// # use std::os::fd::OwnedFd;
/// # fn demo(fd: OwnedFd) -> OwnedFd {
/// match Probably::new(fd).into_client() {
///     Ok(client) => { /* it was a door */ todo!() }
///     // Not a door. We still have the descriptor.
///     Err(e) => e.fd,
/// }
/// # }
/// ```
///
/// Note that turning this into a `Box<dyn Error>` — which `?` will do
/// in a function returning one — drops the descriptor along with
/// everything else. Take [`fd`](NotADoor::fd) out first if you want to
/// keep it.
#[derive(Debug)]
pub struct NotADoor {
    /// Your descriptor, returned. Still open, still yours.
    pub fd: OwnedFd,
    /// What was wrong with it.
    pub reason: NotADoorReason,
}

impl NotADoor {
    /// Take the descriptor back and throw the reason away.
    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

impl fmt::Display for NotADoor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}; the descriptor was handed back", self.reason)
    }
}

impl std::error::Error for NotADoor {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.reason)
    }
}

/// Why a descriptor could not be used as a door.
///
/// Returned on its own by the borrowing constructors on
/// [`BorrowedClient`], where there is no descriptor to hand back: the
/// caller never gave one up. The owning constructors wrap it in
/// [`NotADoor`], which does hand it back.
///
/// [`BorrowedClient`]: crate::BorrowedClient
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotADoorReason {
    /// It is not a door.
    ///
    /// `door_info(3C)` answers only for a door and fails with `EBADF`
    /// for anything else, so that call is the test, and every
    /// constructor makes it. The errno is what it reported.
    ///
    /// There is no cheaper test. The attributes the kernel delivers
    /// with a descriptor cannot tell a door from a pipe; see
    /// [`DescAttributes`](crate::DescAttributes).
    NotADoor(Errno),
    /// It is a door, but it has been revoked.
    ///
    /// A revoked door answers nothing. Every call to it fails. So the
    /// descriptor comes back now, while the caller still has somewhere
    /// to put it, rather than on the first call.
    Revoked,
    /// It is a live door, but it was created with `DOOR_REFUSE_DESC`,
    /// and the caller asked for a client that carries descriptors.
    ///
    /// Only the descriptor-carrying constructors report this. A door
    /// that refuses descriptors is perfectly good for plain calls.
    RefusesDescriptors,
}

impl fmt::Display for NotADoorReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotADoorReason::NotADoor(errno) => write!(
                f,
                "this descriptor is not a door: door_info failed with \
                 errno {}",
                errno.get()
            ),
            NotADoorReason::Revoked => {
                f.write_str("this door has been revoked and answers nothing")
            }
            NotADoorReason::RefusesDescriptors => f.write_str(
                "this door was created with DOOR_REFUSE_DESC, so it \
                 cannot carry descriptors",
            ),
        }
    }
}

impl std::error::Error for NotADoorReason {}

/// A server error that can be written into a reply.
///
/// Blanket-implemented for every `E: Display`, so most users never
/// name this trait. Implement it directly when the encoding matters —
/// a `postcard` payload the client will decode, say, rather than the
/// `Display` text.
pub trait ErrorReply {
    /// Write this error into the reply buffer.
    fn write_error(
        self,
        out: &mut crate::server::ReplyBuf,
    ) -> Result<(), ReplyTooBig>;
}

impl<E: fmt::Display> ErrorReply for E {
    fn write_error(
        self,
        out: &mut crate::server::ReplyBuf,
    ) -> Result<(), ReplyTooBig> {
        use std::fmt::Write as _;
        // Writing through fmt::Write means a Display impl that panics
        // is the user's problem, not a silent truncation. The buffer
        // records an overflow rather than growing past its cap.
        let _ = write!(out, "{self}");
        out.overflow().map_or(Ok(()), Err)
    }
}
