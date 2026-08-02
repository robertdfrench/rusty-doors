// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The bridge between the kernel's C calling convention and a Rust
//! server procedure.
//!
//! # Why this is delicate
//!
//! `door_return(3C)` does not return on success. Control leaves the
//! thread and reappears somewhere else entirely: the kernel enters the
//! server procedure with no return address, so the stack frame this
//! code is standing on simply stops existing.
//!
//! Nothing is unwound. No destructor runs. A `Vec` alive at that
//! moment leaks its allocation, a `MutexGuard` leaves its mutex locked
//! forever, and an `OwnedFd` leaks a descriptor — once per call, until
//! the process dies.
//!
//! So the trampoline is written in two scopes.
//!
//! **Scope 1** is ordinary Rust. The cookie is resolved, the request
//! is built, the user's function runs inside `catch_unwind`, and the
//! reply is written into a [`ReplyBuf`]. Everything here has
//! destructors and all of them run, because scope 1 ends with a plain
//! `}`.
//!
//! **Scope 2** begins after that brace. Only plain integers and raw
//! pointers are alive: no `Vec`, no guard, no `OwnedFd`, nothing that
//! owns anything. `door_return` is called here. If it never comes
//! back, nothing was leaked, because there was nothing left to leak.
//!
//! [`ReplyBuf`] is built for exactly this. Its bytes live either in an
//! inline array in this frame or in a buffer owned by the thread, and
//! neither needs freeing.
//!
//! # If `door_return` does come back
//!
//! Then it failed, and we are still on the same frame. Rule 4.2.4 of
//! `GOALS.md` says the reply descriptors may be re-wrapped and closed.
//! That is not an assumption — `experiments/matrix.c` measures it: in
//! every failure reachable from the safe API the descriptors survived
//! and still referred to the same file. So closing them here is a
//! single close, not a double one.
//!
//! After that we try once more with an empty reply, to release the
//! server thread politely. If even that fails there is nothing sane
//! left to do, and falling off the end of an `extern "C"` function is
//! undefined behaviour, so we `abort`.

use crate::descriptor::{DescriptorPolicy, ReceivedFd};
use crate::error::{ErrorReply, ServerFault, StatusTag};
use crate::server::cookie;
use crate::server::reply_buf::ReplyBuf;
use crate::server::request::Request;
use crate::sys;
use crate::types::door_desc_t;
use std::ffi::{c_char, c_uint, c_void};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::panic::{catch_unwind, AssertUnwindSafe};

/// How many descriptors a single reply may carry.
///
/// Fixed rather than a `Vec` on purpose. The array crosses into scope
/// 2, where anything with a destructor is forbidden, and a fixed array
/// of `i32` has none.
pub const MAX_REPLY_DESCRIPTORS: usize = 16;

/// How a reply is framed on the wire.
///
/// # Why there is a choice
///
/// Two peers that both use this crate can afford one extra byte in
/// front of every reply. That byte is how the client tells "the
/// server returned an error" from "the server returned these bytes"
/// (`GOALS.md` §3.9).
///
/// A door server written in C never writes that byte, and a C client
/// never reads it. If we always wrote it, this crate could only ever
/// talk to itself. Doors are an operating system facility with other
/// users, so the framing has to be a choice, made once, when the door
/// is built (`GOALS.md` §6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyProtocol {
    /// Put the §3.9 status byte in front of every reply.
    ///
    /// The default. Keep it when the other side also uses this crate:
    /// it is what carries user errors, panics and a lost cookie back
    /// to the caller.
    Tagged,
    /// Send exactly the bytes the server procedure produced.
    ///
    /// Nothing is added and nothing is removed. Use it when the
    /// caller is not this crate.
    ///
    /// # There is no way to say "this failed"
    ///
    /// An untagged reply has no room for a status, so:
    ///
    /// * `Ok(bytes)` sends those bytes, exactly.
    /// * `Err(e)` sends the [`ErrorReply`] encoding of `e` with no
    ///   marker at all. The caller cannot tell it apart from a
    ///   success. If your caller must learn about failures, put that
    ///   in your own reply format, exactly as a C door server does.
    /// * A panic, or a cookie that no longer resolves, sends a reply
    ///   of zero bytes. That is the only thing left to send.
    Untagged,
}

/// What a server procedure produced, before it becomes bytes.
///
/// Kept separate from the reply encoding so that the shapes in
/// `GOALS.md` §3.3 can each build one in their own way.
pub struct Outcome<E> {
    /// The reply payload, on success.
    pub data: Result<Vec<u8>, E>,
    /// Descriptors to send back. The four shapes in §3.3 never set
    /// this, but the trampoline handles it so that rule 4.2.4 is
    /// implemented once, here, rather than in each future shape.
    pub descriptors: Vec<OwnedFd>,
}

impl<E> Outcome<E> {
    /// The common case: some bytes, no descriptors.
    pub fn bytes(data: Result<Vec<u8>, E>) -> Self {
        Outcome {
            data,
            descriptors: Vec::new(),
        }
    }
}

/// Run one door invocation and never come back.
///
/// The request is handed to `f` exactly as it arrived. This crate
/// never adds anything to a request and never reads anything off the
/// front of one, under either protocol. Only replies are framed.
///
/// # Safety
///
/// Must be called only from an `extern "C"` function the kernel
/// registered with `door_create`, with that function's own arguments
/// passed through unchanged.
///
/// # Panics
///
/// Never. A panic in `f` is caught, because unwinding out of an
/// `extern "C"` frame is undefined behaviour (`GOALS.md` §12.4).
/// Under [`ReplyProtocol::Tagged`] the panic becomes a §3.9 tag 2
/// reply; under [`ReplyProtocol::Untagged`] it becomes a reply of
/// zero bytes, because an untagged reply has no way to say more.
//
// Five of these arguments are not ours to pick. `cookie`, `argp`,
// `arg_size`, `dp` and `n_desc` are the server procedure signature the
// kernel calls with, and they are passed straight through. Only the
// last three -- the reply limit, the framing and the user's function
// -- are this crate's own. Packing the first five into a struct would
// hide the kernel's calling convention behind a name for no gain.
#[allow(clippy::too_many_arguments)]
pub unsafe fn run<S, D, F, E>(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut door_desc_t,
    n_desc: c_uint,
    reply_limit: usize,
    protocol: ReplyProtocol,
    f: F,
) -> !
where
    D: DescriptorPolicy,
    F: FnOnce(&S, Request<'_, D>) -> Outcome<E>,
    S: Send + Sync + 'static,
    E: ErrorReply,
{
    // Hand the thread's spill buffer back before taking it again.
    //
    // ReplyBuf has no destructor — that is the whole point of it — so
    // it cannot return the buffer itself. Entry is the one place where
    // giving it back is provably safe: if the previous call's
    // door_return succeeded, the frame that ReplyBuf lived in no
    // longer exists, and control never returns to it. Without this,
    // every spilled reply would allocate a fresh buffer.
    //
    // SAFETY: no ReplyBuf from an earlier call on this thread is
    // reachable any more, for the reason just given.
    crate::server::reply_buf::reclaim_spill();

    let mut out = ReplyBuf::with_limit(reply_limit);
    let mut reply_fds: [RawFd; MAX_REPLY_DESCRIPTORS] =
        [-1; MAX_REPLY_DESCRIPTORS];
    let mut n_reply_fds: usize = 0;

    // ---------------- scope 1: destructors live here ----------------
    {
        // Never dereference the cookie directly. The lookup is what
        // makes a stale cookie return a fault instead of touching
        // freed memory.
        match cookie::resolve_or_fault::<S>(cookie) {
            Err(fault) => {
                // `f` is never called on this path, so drop it here.
                // It is a closure and may own something; the other
                // arm hands it to catch_unwind, which drops it in
                // this scope too. Either way nothing of it survives
                // into scope 2.
                drop(f);
                write_fault(&mut out, protocol, fault)
            }
            Ok(state) => {
                // Take ownership of any descriptors the caller sent,
                // so they are closed even if the user's function
                // ignores them or panics.
                let received = collect_descriptors(dp, n_desc);

                // `from_raw` re-checks the DOOR_UNREF_DATA sentinel
                // itself, so passing `false` here is a hint, not a
                // promise. Dereferencing address 1 is the failure mode
                // it is guarding against.
                let request = Request::<'_, D>::from_raw(
                    argp.cast(),
                    arg_size,
                    received,
                    false,
                );

                // AssertUnwindSafe: on the unwind path we abandon both
                // `state` and `request` entirely and reply with a
                // fault, so a half-updated value cannot be observed
                // afterwards.
                let caught =
                    catch_unwind(AssertUnwindSafe(|| f(&state, request)));

                match caught {
                    Err(payload) => {
                        // Drop the panic payload here, in scope 1. It
                        // is a Box<dyn Any> and must not be alive when
                        // door_return runs.
                        drop(payload);
                        write_fault(&mut out, protocol, ServerFault::Panicked);
                    }
                    Ok(outcome) => {
                        n_reply_fds = stage_descriptors(
                            outcome.descriptors,
                            &mut reply_fds,
                        );
                        write_outcome(&mut out, protocol, outcome.data);
                    }
                }
            }
        }

        // A reply that overflowed its limit is not a reply. Replace it
        // with a fault the client can understand, rather than sending
        // a truncated payload that would decode as nonsense.
        if out.overflow().is_some() {
            out.clear();
            write_fault(&mut out, protocol, ServerFault::ReplyTooBig);
        }
    }
    // ---------------- scope 2: no destructors past here -------------
    //
    // Live values from here on: `out` (a ReplyBuf, which owns no
    // heap and has no Drop), `reply_fds` (an array of i32),
    // `n_reply_fds` (a usize), and this function's own arguments,
    // which are raw pointers, integers and `protocol`. A
    // ReplyProtocol is a plain Copy enum with no destructor, so it
    // may cross the line like the integers do. `f` was dropped in
    // scope 1 on both paths. Nothing else is alive, and nothing that
    // is alive owns anything.

    // as_slice() hands back a plain slice, not a guard: ReplyBuf
    // stores a raw pointer into either its own inline array or the
    // thread's spill area. Neither needs freeing, and ReplyBuf itself
    // has no Drop, so both may cross into scope 2.
    let reply = out.as_slice();
    let data_ptr = reply.as_ptr();
    let data_len = reply.len();

    let mut descs: [door_desc_t; MAX_REPLY_DESCRIPTORS] = std::mem::zeroed();
    for i in 0..n_reply_fds {
        descs[i].d_attributes =
            doors_sys::DOOR_DESCRIPTOR | doors_sys::DOOR_RELEASE;
        descs[i].d_data.d_desc.d_descriptor = reply_fds[i];
    }

    let desc_ptr = if n_reply_fds == 0 {
        std::ptr::null()
    } else {
        descs.as_ptr()
    };

    let _ = sys::door_return(
        data_ptr as *const c_char,
        data_len,
        desc_ptr,
        n_reply_fds as c_uint,
    );

    // Only reachable because door_return failed, which means the
    // descriptors were not consumed. Close them exactly once.
    //
    // The index is on purpose. This is scope 2, where the rule at the
    // top of the file says only integers and raw pointers may be
    // alive: the door_return below may never come back, so nothing
    // that needs cleaning up can be left standing. A slice iterator
    // would very likely be fine, but "very likely" is not the standard
    // this scope is held to, and an index keeps the rule checkable by
    // eye.
    #[allow(clippy::needless_range_loop)]
    for i in 0..n_reply_fds {
        // SAFETY: measured in experiments/matrix.c -- a failed
        // door_return leaves every descriptor open and pointing at the
        // same file. We are the only owner, so this is a single close.
        drop(OwnedFd::from_raw_fd(reply_fds[i]));
    }

    // Try once more with nothing attached, to release this server
    // thread back to the pool.
    let _ = sys::door_return(std::ptr::null(), 0, std::ptr::null(), 0);

    // Falling off the end of an extern "C" fn is undefined behaviour,
    // and there is no reply left to send. GOALS.md §4.2 rule 1.
    std::process::abort()
}

/// Take ownership of the descriptors a caller sent.
///
/// Wrapping them in [`ReceivedFd`] means they are closed when the
/// request is dropped, including on the panic path. A server that
/// simply ignored them would otherwise leak one descriptor per call.
unsafe fn collect_descriptors(
    dp: *mut door_desc_t,
    n_desc: c_uint,
) -> Vec<ReceivedFd> {
    if dp.is_null() || n_desc == 0 {
        return Vec::new();
    }
    let slice =
        std::slice::from_raw_parts(dp as *const door_desc_t, n_desc as usize);
    slice.iter().map(|d| ReceivedFd::from_desc(d)).collect()
}

/// Move reply descriptors out of their `OwnedFd`s into raw integers.
///
/// This is the hand-off into scope 2. After this the `OwnedFd`s are
/// gone, so nothing will close these descriptors implicitly; the
/// trampoline closes them by hand if `door_return` comes back.
///
/// Descriptors past [`MAX_REPLY_DESCRIPTORS`] are closed here rather
/// than silently dropped, so they cannot leak.
fn stage_descriptors(
    fds: Vec<OwnedFd>,
    out: &mut [RawFd; MAX_REPLY_DESCRIPTORS],
) -> usize {
    use std::os::fd::IntoRawFd;

    let mut n = 0;
    for fd in fds {
        if n < MAX_REPLY_DESCRIPTORS {
            out[n] = fd.into_raw_fd();
            n += 1;
        } else {
            // Over the cap. Dropping closes it, which is the honest
            // outcome: better one closed descriptor than a leak.
            drop(fd);
        }
    }
    n
}

/// Write an infrastructure failure: a panic, a lost cookie, or a
/// reply that did not fit.
///
/// Tagged, this is a §3.9 tag 2 reply. It carries only the
/// discriminant. A panic message can contain anything the server had
/// in scope, and the caller is on the other side of a trust boundary.
///
/// Untagged, the reply is empty. There is no status byte to put a
/// fault in, and inventing one would send the caller bytes that look
/// like a real reply. Zero bytes is the one answer that cannot be
/// mistaken for data. The caller still gets an answer, so it is not
/// left waiting.
fn write_fault(out: &mut ReplyBuf, protocol: ReplyProtocol, f: ServerFault) {
    out.clear();
    match protocol {
        ReplyProtocol::Tagged => {
            let _ = out.write_bytes(&[StatusTag::Fault as u8, f.as_byte()]);
        }
        ReplyProtocol::Untagged => {}
    }
}

/// Write what the server procedure returned.
///
/// Tagged, this is a §3.9 tag 0 or tag 1 reply.
///
/// Untagged, the bytes go out on their own. An error goes out on its
/// own too, with nothing to mark it as an error, because an untagged
/// protocol has no error channel; see [`ReplyProtocol::Untagged`].
fn write_outcome<E: ErrorReply>(
    out: &mut ReplyBuf,
    protocol: ReplyProtocol,
    data: Result<Vec<u8>, E>,
) {
    match data {
        Ok(bytes) => {
            if protocol == ReplyProtocol::Tagged {
                let _ = out.write_bytes(&[StatusTag::Ok as u8]);
            }
            let _ = out.write_bytes(&bytes);
        }
        Err(e) => {
            if protocol == ReplyProtocol::Tagged {
                let _ = out.write_bytes(&[StatusTag::UserError as u8]);
            }
            // The blanket impl records overflow rather than failing,
            // and the caller checks out.overflow() afterwards.
            let _ = e.write_error(out);
        }
    }
}
