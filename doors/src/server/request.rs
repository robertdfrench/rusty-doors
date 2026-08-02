// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What a server procedure receives: the request.
//!
//! The trampoline builds one of these from the raw arguments the kernel
//! hands a door server procedure, then passes it to the user function.
//! Nothing here is reachable from a client.

use crate::descriptor::{Descriptors, NoDescriptors, ReceivedFd};
use crate::error::Error;
use crate::sys;
use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;

/// One call, as the server sees it.
///
/// `D` says whether this request may carry descriptors. It is a
/// typestate, not a runtime flag:
///
/// - [`NoDescriptors`] has **no** method that reaches a descriptor.
/// - [`Descriptors`] adds [`descriptors`](Request::descriptors).
///
/// The macro picks `D` from the `refuse_desc` option on `#[door(...)]`.
/// A door built with `refuse_desc` gets `DOOR_REFUSE_DESC`, so the
/// kernel never delivers a descriptor to it. The typestate mirrors that
/// in the type system, which is why the crate adds no runtime check: a
/// server that refuses descriptors cannot even name the method that
/// would read one. Refusing is enforced by the compiler.
///
/// The lifetime `'a` is the length of one invocation. The request data
/// lives on the server thread's stack for the duration of the call and
/// is gone the moment the procedure returns, so [`data`](Request::data)
/// borrows and never copies.
///
/// This type is deliberately neither `Send` nor `Sync`. Its credentials
/// come from `door_ucred(3C)`, which only means something on the door
/// server thread that is running the call. Moving a `Request` to
/// another thread would make [`peer`](Request::peer) meaningless.
pub struct Request<'a, D = NoDescriptors> {
    /// Borrowed from the kernel's argument area. Empty for an
    /// unreferenced notification; see [`Request::from_raw`].
    data: &'a [u8],
    /// Descriptors that came with the call. Always empty when
    /// `D = NoDescriptors`, because the door refused them.
    descriptors: Vec<ReceivedFd>,
    /// True when this is an unreferenced notification, not a real call.
    unreferenced: bool,
    /// The `ucred_t *` from the last [`peer`](Request::peer) call, kept
    /// so the next one can reuse the allocation. `None` until someone
    /// asks. Freed in `Drop`.
    ucred: Option<*mut c_void>,
    _d: PhantomData<D>,
}

impl<'a, D> Request<'a, D> {
    /// Build a request from the raw server procedure arguments.
    ///
    /// # The address-1 trap
    ///
    /// This is the most dangerous code in the file. When a door has
    /// `DOOR_UNREF` or `DOOR_UNREF_MULTI` and its last client reference
    /// goes away, the kernel invokes the server procedure with `argp`
    /// set to `DOOR_UNREF_DATA`. That value is **the literal address
    /// 1**. It is not a pointer to anything. Reading one byte from it
    /// would fault and kill the process.
    ///
    /// So this function compares `argp` against the sentinel *before*
    /// it ever builds a slice, and on a match produces an empty
    /// `data()` and sets `is_unreferenced()`. The comparison is done on
    /// integers, because 1 is not an address we are allowed to compute
    /// with. Never move a dereference above that check.
    ///
    /// # Safety
    ///
    /// The caller must be a door server procedure, running on the
    /// thread the kernel called, and:
    ///
    /// - `argp` is either the `DOOR_UNREF_DATA` sentinel, or null, or
    ///   points at `arg_size` readable bytes.
    /// - Those bytes stay alive and unchanged for `'a`. The caller
    ///   chooses `'a`, and it must not outlive the invocation.
    /// - `descriptors` are the ones the kernel just delivered, and no
    ///   one else will close them.
    pub(crate) unsafe fn from_raw(
        argp: *const u8,
        arg_size: usize,
        descriptors: Vec<ReceivedFd>,
        unreferenced: bool,
    ) -> Self {
        // Trust either signal. The caller may already know from the
        // door's flags; we still check the pointer ourselves, because
        // getting this wrong dereferences address 1.
        let unreferenced = unreferenced || is_unref_sentinel(argp);

        let data: &'a [u8] = if unreferenced || argp.is_null() {
            // No payload exists on this path. `from_raw_parts` demands
            // a valid, aligned pointer even for a zero length, so hand
            // back a real empty slice instead of faking one.
            &[]
        } else if arg_size == 0 {
            &[]
        } else {
            // SAFETY: not the sentinel and not null, so by this
            // function's contract there are `arg_size` readable bytes
            // there, alive for `'a`.
            unsafe { std::slice::from_raw_parts(argp, arg_size) }
        };

        Request {
            data,
            descriptors,
            unreferenced,
            ucred: None,
            _d: PhantomData,
        }
    }

    /// The request payload.
    ///
    /// Borrowed from the request, so it cannot outlive the invocation.
    /// Empty for an unreferenced notification.
    pub fn data(&self) -> &[u8] {
        self.data
    }

    /// Who called, according to `door_ucred(3C)`.
    ///
    /// # Why `&mut self`
    ///
    /// Two reasons, and both matter.
    ///
    /// First, reuse. `door_ucred(3C)` fills in a caller-supplied
    /// buffer when it is given one, and allocates a fresh buffer only
    /// when the pointer is null. Keeping that buffer inside the
    /// `Request` lets a second call reuse the first call's allocation.
    /// Handing it out behind `&self` would mean either allocating every
    /// time or hiding the mutation, so `&mut self` says out loud what
    /// is happening.
    ///
    /// Second, timing. The returned [`UCred`] borrows from the
    /// `Request`, so credentials cannot be asked for outside an
    /// invocation. `door_ucred` reports the *current* call on the
    /// *current* door server thread. Away from that, its answer is
    /// stale or absent, and a type that let you ask anyway would be
    /// lying.
    ///
    /// # The cost
    ///
    /// Because the [`UCred`] holds a mutable borrow, you cannot read
    /// [`data`](Request::data) while you hold it. That is on purpose:
    /// it keeps the credential lookup a short, obvious step rather than
    /// something kept alive next to the payload. Copy out the fields
    /// you need — they are all plain integers — and drop the `UCred`.
    ///
    /// ```ignore
    /// let uid = req.peer()?.euid();   // UCred dies at the semicolon
    /// let body = req.data();          // now the borrow is free again
    /// ```
    pub fn peer(&mut self) -> Result<UCred<'_>, Error> {
        // Take the old buffer out, if we have one. A non-null pointer
        // asks door_ucred to reuse it; null asks it to allocate.
        let mut raw = self.ucred.take().unwrap_or(std::ptr::null_mut());

        // SAFETY: `raw` is either null or a `ucred_t *` that
        // door_ucred itself produced and nothing has freed. We are on
        // the door server thread, which is the only place this call
        // means anything.
        let rc = unsafe { sys::door_ucred(&mut raw) };

        if rc < 0 || raw.is_null() {
            // On failure `door_ucred` leaves a buffer we supplied
            // alone, and frees one it allocated itself. Keeping
            // whatever non-null value came back is therefore correct
            // on both paths: we never free something the callee freed,
            // and we never drop the last pointer to something it kept.
            if !raw.is_null() {
                self.ucred = Some(raw);
            }
            return Err(Error::sys("door_ucred"));
        }

        Ok(UCred {
            raw,
            slot: &mut self.ucred,
        })
    }

    /// True when this is an unreferenced notification rather than a
    /// real call.
    ///
    /// The kernel delivers one of these after the last client
    /// descriptor for the door goes away, if the door was created with
    /// `DOOR_UNREF` or `DOOR_UNREF_MULTI`. There is no payload, no
    /// caller and no reply worth sending. See `from_raw` for why this
    /// flag is a safety matter and not a convenience.
    pub fn is_unreferenced(&self) -> bool {
        self.unreferenced
    }
}

impl Request<'_, Descriptors> {
    /// Descriptors the caller sent.
    ///
    /// Only exists on `Request<'_, Descriptors>`. On a door built with
    /// `refuse_desc` the parameter is [`NoDescriptors`] and this method
    /// is not in scope, so there is nothing to forget to check.
    ///
    /// Each [`ReceivedFd`] closes itself on `Drop`, along with the
    /// request.
    pub fn descriptors(&self) -> &[ReceivedFd] {
        &self.descriptors
    }
}

impl<D> fmt::Debug for Request<'_, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("len", &self.data.len())
            .field("descriptors", &self.descriptors.len())
            .field("unreferenced", &self.unreferenced)
            .finish()
    }
}

impl<D> Drop for Request<'_, D> {
    fn drop(&mut self) {
        if let Some(u) = self.ucred.take() {
            // SAFETY: this pointer came from door_ucred, we are its
            // only owner, and taking it out of the Option means no
            // second free can happen.
            unsafe { sys::ucred_free(u) };
        }
    }
}

/// Is this `argp` the kernel's unreferenced marker?
///
/// `DOOR_UNREF_DATA` is the literal address 1. Compare it as an
/// integer: 1 is not a real address, and doing pointer arithmetic on it
/// is not something the language promises to keep meaningful.
// `clippy::ptr_eq` wants `std::ptr::eq` here. That would not compile:
// `argp` is a `*const u8` and `DOOR_UNREF_DATA` is a `*const c_void`.
// The integer comparison is the point, not an oversight.
#[allow(clippy::ptr_eq)]
fn is_unref_sentinel(argp: *const u8) -> bool {
    argp as usize == doors_sys::DOOR_UNREF_DATA as usize
}

/// The credentials of whoever made the call.
///
/// Borrowed from the [`Request`], for the reasons in
/// [`Request::peer`]. Every field is a plain integer, so copy out what
/// you need and let this value go.
///
/// The `ucred_t` behind it is not freed here in the normal case. It
/// goes back to the `Request` when this value drops, so the next
/// [`peer`](Request::peer) call reuses the same allocation. The
/// `Request` frees it at the end of the invocation.
pub struct UCred<'a> {
    /// The live `ucred_t *`. Never null.
    raw: *mut c_void,
    /// Where to put it back. Borrowing this from the `Request` is what
    /// stops credentials from being read outside an invocation.
    slot: &'a mut Option<*mut c_void>,
}

impl UCred<'_> {
    /// The caller's effective user id.
    pub fn euid(&self) -> libc::uid_t {
        // SAFETY: `raw` is a live ucred_t we own for `'a`.
        unsafe { sys::ucred_geteuid(self.raw) }
    }

    /// The caller's effective group id.
    pub fn egid(&self) -> libc::gid_t {
        // SAFETY: `raw` is a live ucred_t we own for `'a`.
        unsafe { sys::ucred_getegid(self.raw) }
    }

    /// The caller's real user id.
    pub fn ruid(&self) -> libc::uid_t {
        // SAFETY: `raw` is a live ucred_t we own for `'a`.
        unsafe { sys::ucred_getruid(self.raw) }
    }

    /// The caller's real group id.
    pub fn rgid(&self) -> libc::gid_t {
        // SAFETY: `raw` is a live ucred_t we own for `'a`.
        unsafe { sys::ucred_getrgid(self.raw) }
    }

    /// The caller's process id.
    ///
    /// A pid is only useful while the caller is still blocked in
    /// `door_call`. Once it returns, the process may exit and the
    /// number may be reused by something else. Do not store it and
    /// trust it later.
    pub fn pid(&self) -> libc::pid_t {
        // SAFETY: `raw` is a live ucred_t we own for `'a`.
        unsafe { sys::ucred_getpid(self.raw) }
    }
}

impl fmt::Debug for UCred<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UCred")
            .field("euid", &self.euid())
            .field("egid", &self.egid())
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

impl Drop for UCred<'_> {
    fn drop(&mut self) {
        // Hand the allocation back to the `Request`, so the next
        // `peer()` call fills it in again instead of asking the system
        // for a new one.
        //
        // `replace` returns whatever was in the slot. Through the safe
        // API that is always `None`: `peer()` emptied the slot and the
        // mutable borrow stopped anyone from refilling it while this
        // value was alive. If it somehow is not, freeing the other one
        // keeps exactly one allocation, which is the invariant
        // `Request::drop` relies on.
        if let Some(other) = self.slot.replace(self.raw) {
            // SAFETY: a ucred_t from door_ucred, now unreachable from
            // anywhere else, so this frees it exactly once.
            unsafe { sys::ucred_free(other) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests need no live door, so they run on a development
    // machine as well as on illumos. Nothing here calls `peer()`:
    // `door_ucred` only answers on a door server thread, so it belongs
    // in the VM tests of `GOALS.md` §9.2.
    //
    // One thing worth stating that no test can state, because the
    // failure it describes is a compile error and not a value:
    //
    //     req: Request<'_, NoDescriptors>
    //     req.descriptors()      // does not compile: no such method
    //
    // `descriptors()` lives on `impl Request<'_, Descriptors>` only.
    // The proof belongs in the `trybuild` compile-fail suite (§9.1);
    // if it were written here the module would stop compiling.

    /// Address 1 must never be read. It is a marker, not a pointer.
    #[test]
    fn unref_sentinel_is_detected() {
        let sentinel = doors_sys::DOOR_UNREF_DATA as *const u8;
        assert!(is_unref_sentinel(sentinel));
        assert!(!is_unref_sentinel(std::ptr::null()));

        let bytes = [1u8, 2, 3];
        assert!(!is_unref_sentinel(bytes.as_ptr()));
    }

    /// The kernel may report a non-zero size with the sentinel. The
    /// size must be ignored, or we dereference address 1.
    #[test]
    fn sentinel_wins_over_a_nonzero_size() {
        let sentinel = doors_sys::DOOR_UNREF_DATA as *const u8;
        // SAFETY: the sentinel is exactly the input this function
        // exists to recognise, and it never dereferences it.
        let req: Request<'_, NoDescriptors> =
            unsafe { Request::from_raw(sentinel, 64, Vec::new(), false) };

        assert!(req.is_unreferenced());
        assert!(req.data().is_empty());
    }

    /// The caller can also tell us, without us seeing the sentinel.
    #[test]
    fn caller_supplied_unref_flag_is_kept() {
        let bytes = [7u8; 4];
        // SAFETY: `bytes` outlives `req` and holds four readable
        // bytes.
        let req: Request<'_, NoDescriptors> = unsafe {
            Request::from_raw(bytes.as_ptr(), bytes.len(), Vec::new(), true)
        };

        assert!(req.is_unreferenced());
        assert!(req.data().is_empty());
    }

    /// A null pointer and a zero size are both empty, not a crash.
    #[test]
    fn empty_request_has_empty_data() {
        // SAFETY: null with a zero size is allowed by the contract.
        let null_req: Request<'_, NoDescriptors> = unsafe {
            Request::from_raw(std::ptr::null(), 0, Vec::new(), false)
        };
        assert!(null_req.data().is_empty());
        assert!(!null_req.is_unreferenced());

        let bytes = [0u8; 8];
        // SAFETY: a real pointer with a zero size.
        let short_req: Request<'_, NoDescriptors> =
            unsafe { Request::from_raw(bytes.as_ptr(), 0, Vec::new(), false) };
        assert!(short_req.data().is_empty());
    }

    /// A normal call: the payload is borrowed, byte for byte.
    #[test]
    fn data_borrows_the_argument_area() {
        let bytes = *b"hello";
        // SAFETY: `bytes` outlives `req` and holds five readable
        // bytes.
        let req: Request<'_, NoDescriptors> = unsafe {
            Request::from_raw(bytes.as_ptr(), bytes.len(), Vec::new(), false)
        };

        assert_eq!(req.data(), b"hello");
        assert!(!req.is_unreferenced());
    }

    /// The descriptor side compiles and starts empty.
    #[test]
    fn descriptors_start_empty() {
        let bytes = *b"x";
        // SAFETY: `bytes` outlives `req`.
        let req: Request<'_, Descriptors> = unsafe {
            Request::from_raw(bytes.as_ptr(), bytes.len(), Vec::new(), false)
        };

        assert!(req.descriptors().is_empty());
        assert_eq!(req.data(), b"x");
    }
}
