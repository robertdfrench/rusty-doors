// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Descriptors, and the typestate that decides who may touch them.

use crate::types::{door_attr_t, door_desc_t};
use doors_sys::{DOOR_DESCRIPTOR, DOOR_LOCAL, DOOR_RELEASE, DOOR_REVOKED};
use std::fmt;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};

mod sealed {
    pub trait Sealed {}
}

/// Whether a [`Client`] or [`Request`] may carry descriptors.
///
/// This is a typestate, not a runtime flag. A
/// `Client<NoDescriptors>` has no method that sends one and a
/// `Request<'_, NoDescriptors>` has no method that reads one, so the
/// mistake is a compile error rather than a check someone can forget.
///
/// One policy covers both directions. A client that never sends a
/// descriptor still needs [`Descriptors`] to receive one.
///
/// Sealed: the two implementors below are the only ones.
///
/// [`Client`]: crate::Client
/// [`Request`]: crate::server::Request
pub trait DescriptorPolicy: sealed::Sealed {
    /// Whether this policy accepts descriptors. Used by the runtime
    /// paths that must clean up after a peer that ignored the door's
    /// declared attributes.
    const ACCEPTS: bool;
}

/// Descriptors are refused. The default.
///
/// Refusing is the default because accepting costs real cleanup: a
/// descriptor that arrives unwanted still has to be closed, and a
/// caller that forgets leaks a file descriptor per call.
///
/// This refuses both directions. The name sounds like "does not send
/// descriptors", and it also means "cannot receive one": a
/// `Client<NoDescriptors>` handed a descriptor closes it and fails the
/// call with [`CallError::UnexpectedDescriptors`]. A client that only
/// ever reads a descriptor out of a reply still needs
/// [`Client::with_descriptors`].
///
/// [`CallError::UnexpectedDescriptors`]:
///     crate::CallError::UnexpectedDescriptors
/// [`Client::with_descriptors`]: crate::Client::with_descriptors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoDescriptors;

impl sealed::Sealed for NoDescriptors {}
impl DescriptorPolicy for NoDescriptors {
    const ACCEPTS: bool = false;
}

/// Descriptors are accepted, and the holder is responsible for them.
///
/// Both directions again: this is what lets a client send a
/// descriptor, and what lets it receive one. Most clients that need it
/// need it only to read a descriptor out of the reply.
///
/// A door built with
/// [`refuse_descriptors`](crate::DoorBuilder::refuse_descriptors) can
/// never give a client this state, so it can never reply with a
/// descriptor either. Use
/// [`max_descriptors(0)`](crate::DoorBuilder::max_descriptors) to
/// refuse incoming descriptors and still hand one back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptors;

impl sealed::Sealed for Descriptors {}
impl DescriptorPolicy for Descriptors {
    const ACCEPTS: bool = true;
}

/// The kernel's unique identifier for a door.
///
/// Deliberately has no conversion to [`RawFd`]. A `d_id` is not a
/// descriptor — it is a uniquifier the kernel stamps on doors so two
/// descriptors referring to the same door can be recognised as such.
/// Treating one as an fd would close or read something unrelated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DoorId(u64);

impl DoorId {
    pub(crate) fn new(raw: u64) -> Self {
        DoorId(raw)
    }

    /// The raw uniquifier, for logging and comparison.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for DoorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "door#{}", self.0)
    }
}

/// What the kernel said about a descriptor it handed us.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct DescAttributes(door_attr_t);

impl DescAttributes {
    pub(crate) fn new(raw: door_attr_t) -> Self {
        DescAttributes(raw)
    }

    /// The descriptor is a door.
    pub fn is_door(self) -> bool {
        self.0 & DOOR_DESCRIPTOR != 0
    }

    /// The door is local to this process.
    pub fn is_local(self) -> bool {
        self.0 & DOOR_LOCAL != 0
    }

    /// The door has been revoked.
    pub fn is_revoked(self) -> bool {
        self.0 & DOOR_REVOKED != 0
    }

    /// The sender released its reference along with the descriptor.
    pub fn was_released(self) -> bool {
        self.0 & DOOR_RELEASE != 0
    }

    /// The raw attribute word.
    pub fn bits(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for DescAttributes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DescAttributes")
            .field("bits", &format_args!("{:#x}", self.0))
            .field("door", &self.is_door())
            .field("local", &self.is_local())
            .field("revoked", &self.is_revoked())
            .field("released", &self.was_released())
            .finish()
    }
}

/// A descriptor on its way out, to a door or back to a caller.
///
/// The two arms differ in who owns the descriptor afterwards, which is
/// why this is an enum rather than a flag:
///
/// - [`Shared`](SentFd::Shared) borrows. The peer gets its own
///   reference; ours stays valid and this crate never closes it.
/// - [`Released`](SentFd::Released) hands ownership to the kernel. The
///   caller's [`OwnedFd`] is consumed, so it cannot be closed twice.
///
/// `DOOR_DESCRIPTOR` is set on both. There is no way to construct a
/// `SentFd` that omits it.
#[derive(Debug)]
pub enum SentFd<'a> {
    /// Send a reference; keep ours. `DOOR_DESCRIPTOR`.
    Shared(BorrowedFd<'a>),
    /// Send it and give up ownership. `DOOR_DESCRIPTOR | DOOR_RELEASE`.
    Released(OwnedFd),
}

impl<'a> SentFd<'a> {
    /// Borrow a descriptor without giving it up.
    pub fn shared(fd: BorrowedFd<'a>) -> Self {
        SentFd::Shared(fd)
    }

    /// Give a descriptor away.
    pub fn released(fd: OwnedFd) -> Self {
        SentFd::Released(fd)
    }

    /// Take this apart into the raw descriptor and its attributes,
    /// giving up any ownership we had.
    ///
    /// Step 1 of the descriptor dance in `GOALS.md` §6.4: after this,
    /// no `OwnedFd` for a `Released` descriptor exists anywhere, so
    /// whatever the kernel does with it cannot cause a double close.
    /// The caller becomes responsible for re-wrapping it if — and only
    /// if — the call was rejected outright.
    pub(crate) fn into_raw(self) -> (RawFd, door_attr_t, bool) {
        match self {
            SentFd::Shared(fd) => (fd.as_raw_fd(), DOOR_DESCRIPTOR, false),
            SentFd::Released(fd) => {
                (fd.into_raw_fd(), DOOR_DESCRIPTOR | DOOR_RELEASE, true)
            }
        }
    }
}

/// A descriptor that arrived from a door, and is ours to close.
///
/// Closing happens on `Drop`, once. There is no way to get the raw
/// descriptor out and keep the `ReceivedFd` as well.
#[derive(Debug)]
pub struct ReceivedFd {
    fd: OwnedFd,
    door_id: Option<DoorId>,
    attributes: DescAttributes,
}

impl ReceivedFd {
    /// Build one from a `door_desc_t` the kernel filled in.
    ///
    /// # Safety
    ///
    /// `d` must be a descriptor the kernel just delivered, and the
    /// caller must not use its `d_descriptor` again: ownership moves
    /// here.
    pub(crate) unsafe fn from_desc(d: &door_desc_t) -> Self {
        // Read through copies. These fields live in a packed struct,
        // so taking a reference to one is undefined behaviour.
        let attributes = d.d_attributes;
        let raw = d.d_data.d_desc.d_descriptor;
        let id = d.d_data.d_desc.d_id;

        let attrs = DescAttributes::new(attributes);
        ReceivedFd {
            fd: OwnedFd::from_raw_fd(raw),
            // d_id only means anything when the descriptor is a door.
            door_id: attrs.is_door().then(|| DoorId::new(id)),
            attributes: attrs,
        }
    }

    /// Borrow the descriptor.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.fd.as_fd()
    }

    /// Take ownership of the descriptor, consuming this value.
    pub fn into_owned(self) -> OwnedFd {
        self.fd
    }

    /// The door's unique id, when the descriptor is a door.
    pub fn door_id(&self) -> Option<DoorId> {
        self.door_id
    }

    /// What the kernel said about it.
    pub fn attributes(&self) -> DescAttributes {
        self.attributes
    }
}

impl AsRawFd for ReceivedFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
