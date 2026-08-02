// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Serving doors.

pub mod builder;
pub mod cookie;
pub mod reply_buf;
pub mod request;
pub mod trampoline;

pub use builder::DoorBuilder;
pub use reply_buf::ReplyBuf;
pub use request::{Request, UCred};
pub use trampoline::ReplyProtocol;

use crate::error::{Error, RevokeError};
use crate::registry::{self, DoorInner};
use crate::sys;
use crate::types::{door_attr_t, door_info_t};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// What `door_info(3C)` reports about a door.
///
/// The fields are copied out of the raw `door_info_t` rather than
/// borrowed from it. That struct is `#[repr(C, packed(4))]`, so taking
/// a reference to any field of it would be undefined behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorInfo {
    target: libc::pid_t,
    attributes: door_attr_t,
    uniquifier: u64,
}

impl DoorInfo {
    /// The process serving this door.
    pub fn target_pid(&self) -> libc::pid_t {
        self.target
    }

    /// The raw attribute word.
    pub fn attributes(&self) -> door_attr_t {
        self.attributes
    }

    /// The kernel's unique number for this door.
    pub fn uniquifier(&self) -> u64 {
        self.uniquifier
    }

    /// The door lives in this process.
    pub fn is_local(&self) -> bool {
        self.attributes & doors_sys::DOOR_LOCAL != 0
    }

    /// The door has been revoked and will refuse further calls.
    pub fn is_revoked(&self) -> bool {
        self.attributes & doors_sys::DOOR_REVOKED != 0
    }

    /// The door refuses descriptors from callers.
    pub fn refuses_descriptors(&self) -> bool {
        self.attributes & doors_sys::DOOR_REFUSE_DESC != 0
    }

    /// The door currently has no client references.
    pub fn is_unreferenced(&self) -> bool {
        self.attributes & doors_sys::DOOR_IS_UNREF != 0
    }
}

/// Ask the kernel about a door descriptor.
pub(crate) fn door_info_for(fd: std::os::fd::RawFd) -> Result<DoorInfo, Error> {
    // SAFETY: zeroed is a valid door_info_t and the kernel fills it in.
    let mut raw: door_info_t = unsafe { std::mem::zeroed() };
    // SAFETY: raw is live for the call.
    let rc = unsafe { sys::door_info(fd, &mut raw) };
    if rc < 0 {
        return Err(Error::sys("door_info"));
    }

    // Copy each field out by value. door_info_t is packed, so `&raw.di_proc`
    // would be a misaligned reference.
    let target = raw.di_target;
    let attributes = raw.di_attributes;
    let uniquifier = raw.di_uniquifier;

    Ok(DoorInfo {
        target,
        attributes,
        uniquifier,
    })
}

/// A door this process serves.
///
/// Dropping a `Door` revokes it and removes any paths it was attached
/// to — but only if this process still owns it. After a `fork` the
/// child holds a `Door` that refers to the parent's door, and tearing
/// that down would remove a path the parent is still serving. See
/// `GOALS.md` §7 and [`crate::fork`].
///
/// There is no separate "jamb" type. The attached paths live here,
/// because they share the door's lifetime exactly: a path that
/// outlived its door would be a path leading nowhere.
///
/// The descriptor is private. `Door` implements neither `AsRawFd` nor
/// `IntoRawFd` (`GOALS.md` §12.6): handing it out would let someone
/// close it while the registry still believed it was open.
pub struct Door<S>
where
    S: Send + Sync + 'static,
{
    inner: Arc<DoorInner>,
    registry_key: usize,
    attached: Vec<PathBuf>,
    /// Taken by [`revoke`](Door::revoke), so `Drop` knows not to
    /// repeat the teardown.
    handle: Option<cookie::Ticket<S>>,
}

impl<S: Send + Sync + 'static> Door<S> {
    /// Start building a door around some state.
    ///
    /// The state becomes the door's cookie. Every invocation gets a
    /// reference to it.
    pub fn builder(state: S) -> DoorBuilder<S> {
        DoorBuilder::new(state)
    }

    pub(crate) fn from_parts(
        inner: Arc<DoorInner>,
        registry_key: usize,
        handle: cookie::Ticket<S>,
    ) -> Self {
        Door {
            inner,
            registry_key,
            attached: Vec::new(),
            handle: Some(handle),
        }
    }

    /// What the kernel knows about this door.
    pub fn info(&self) -> Result<DoorInfo, Error> {
        self.guard()?;
        door_info_for(self.inner.raw_fd())
    }

    /// Make the door reachable at a path.
    ///
    /// The path must already exist as a file; `fattach(3C)` covers it
    /// the way a mount covers a directory.
    pub fn attach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        self.guard()?;
        let path = path.as_ref();
        let c = cpath(path)?;

        // SAFETY: c is NUL-terminated and outlives the call.
        let rc = unsafe { sys::fattach(self.inner.raw_fd(), c.as_ptr()) };
        if rc < 0 {
            return Err(Error::sys("fattach"));
        }
        self.attached.push(path.to_path_buf());
        Ok(())
    }

    /// Stop serving the door at a path.
    ///
    /// Touches no descriptor and no registry entry, so it is safe to
    /// call while other threads are serving calls.
    pub fn detach<P: AsRef<Path>>(&mut self, path: P) -> Result<(), Error> {
        if self.inner.is_disowned() {
            return Err(Error::Disowned);
        }
        let path = path.as_ref();
        let c = cpath(path)?;

        // SAFETY: c is NUL-terminated and outlives the call.
        let rc = unsafe { sys::fdetach(c.as_ptr()) };
        if rc < 0 {
            return Err(Error::sys("fdetach"));
        }
        self.attached.retain(|p| p != path);
        Ok(())
    }

    /// Revoke the door and take the state back.
    ///
    /// Revokes, waits for every call already in flight to finish, then
    /// drops the door and returns the state.
    ///
    /// # This blocks
    ///
    /// If a server procedure never returns, neither does this. That is
    /// deliberate: the alternative is to free the state while a call is
    /// still using it, which is a use-after-free. A hung server
    /// procedure is a bug in the procedure, and blocking makes it
    /// visible instead of turning it into memory corruption.
    pub fn revoke(mut self) -> Result<S, RevokeError> {
        if self.inner.is_disowned() || !self.inner.is_owner() {
            return Err(RevokeError::Disowned);
        }

        // The registry does the revoke, under its own lock, so the
        // entry is gone before the descriptor is. `door_revoke` IS the
        // release: it closes the descriptor. There must be no close
        // after this line (`docs/DESIGN.md` Appendix E).
        if let Some(errno) = registry::deregister_and_release(
            &self.inner,
            self.registry_key,
            true,
        ) {
            return Err(RevokeError::Sys { errno });
        }

        // Drain. After door_revoke no new call can start, so this
        // counter only goes down.
        while self.inner.in_flight.load(Ordering::Acquire) > 0 {
            std::thread::yield_now();
        }

        self.teardown_paths();

        let handle = self.handle.take().expect("ticket taken twice");
        let state = cookie::uninstall(handle);

        // Forget BEFORE anything that can return early.
        //
        // An earlier version put this after a `?` on the line above.
        // When that arm was taken, `Drop` still ran and called
        // `deregister_and_release` a second time with a key whose slot
        // another door had since been given — closing a descriptor
        // that door was still using. It showed up as `fattach`
        // failing with EBADF in an unrelated test.
        std::mem::forget(self);

        let state = state.ok_or(RevokeError::Disowned)?;

        // The drain above waited for every in-flight call to finish,
        // and each of those held a clone. So this is normally the last
        // reference. It is not if the user kept a clone of their own,
        // and in that case we cannot hand back an owned S.
        Arc::try_unwrap(state).map_err(|_| RevokeError::StateStillShared)
    }

    /// `Err(Disowned)` when this process must not act on the door.
    fn guard(&self) -> Result<(), Error> {
        if self.inner.is_disowned() || !self.inner.is_owner() {
            return Err(Error::Disowned);
        }
        Ok(())
    }

    fn teardown_paths(&mut self) {
        for path in self.attached.drain(..) {
            let Ok(c) = cpath(&path) else { continue };
            // SAFETY: c outlives both calls.
            unsafe {
                sys::fdetach(c.as_ptr());
            }
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl<S: Send + Sync + 'static> Drop for Door<S> {
    fn drop(&mut self) {
        // A child that forked away from this door must not revoke it,
        // must not detach its paths, and must not unlink them. The
        // parent is still serving there. Two independent checks,
        // because vfork and forkall do not run atfork handlers the
        // same way (GOALS.md §7.3).
        let ours = !self.inner.is_disowned() && self.inner.is_owner();

        // `handle` is Some exactly while this door still owns its
        // registry entry. `revoke` takes it before tearing down, so a
        // `None` here means the teardown already happened and
        // repeating it would release a descriptor number that now
        // belongs to somebody else.
        if let Some(handle) = self.handle.take() {
            // Deregistration and the release happen either way. The
            // registry must not keep an entry pointing at a descriptor
            // we are done with, disowned or not (GOALS.md §7.1).
            //
            // `ours` picks how the descriptor goes away. For a door we
            // own that is `door_revoke`, which CLOSES the descriptor
            // by itself — so nothing here may close it again. For a
            // door a fork disowned it is a plain close, because
            // revoking would destroy the door the parent still serves
            // (`docs/DESIGN.md` Appendix E).
            let _ = registry::deregister_and_release(
                &self.inner,
                self.registry_key,
                ours,
            );

            // Paths come down after the door stops answering, and only
            // for a door we own. A forked child must not fdetach or
            // unlink a path its parent is still serving.
            if ours {
                self.teardown_paths();
            }

            cookie::uninstall(handle);
        }
    }
}

impl<S: Send + Sync + 'static> std::fmt::Debug for Door<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Door")
            .field("attached", &self.attached)
            .field("disowned", &self.inner.is_disowned())
            .field("owner", &self.inner.is_owner())
            .finish_non_exhaustive()
    }
}

fn cpath(path: &Path) -> Result<CString, Error> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::PathHasNul)
}
