// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The `extern "C"` declarations, and the one wrapper.
//!
//! The `raw` module below is private: it is where the bindings are
//! *declared*, spelled exactly as C spells them, returning `c_int`.
//! Everything except `door_return` is then re-exported unchanged, so
//! `doors_sys::door_call` really is the C function and not a
//! reinterpretation of it.
//!
//! [`door_return`] is the single exception, for the reason given on
//! its own doc comment.

use crate::types::*;
use core::ffi::{c_char, c_int, c_uint};
use libc::size_t;

/// The declarations. Private on purpose -- see the module docs.
mod raw {
    use super::*;
    use core::ffi::{c_long, c_void};
    use libc::size_t;

    extern "C" {
        // --- <door.h> ---

        /// Create a door descriptor for `server_procedure`.
        ///
        /// See [`door_create(3C)`][1]. `attributes` is a mask of
        /// [`DOOR_CREATE_MASK`](crate::DOOR_CREATE_MASK) bits.
        ///
        /// [1]: https://illumos.org/man/3C/door_create
        pub fn door_create(
            server_procedure: door_server_procedure_t,
            cookie: *mut c_void,
            attributes: c_uint,
        ) -> c_int;

        /// Create a door with caller-controlled server threads.
        ///
        /// See [`door_xcreate(3C)`][1]. This is the entry point that
        /// lets a caller choose the server thread stack size, which
        /// matters because request data lands on that stack.
        ///
        /// [1]: https://illumos.org/man/3C/door_xcreate
        pub fn door_xcreate(
            server_procedure: door_server_procedure_t,
            cookie: *mut c_void,
            attributes: c_uint,
            thr_create_func: Option<door_xcreate_server_func_t>,
            thr_setup_func: Option<door_xcreate_thrsetup_func_t>,
            thr_create_cookie: *mut c_void,
            nthread: c_int,
        ) -> c_int;

        /// Revoke access to a door.
        ///
        /// See [`door_revoke(3C)`][1]. Calls already in progress are
        /// allowed to finish.
        ///
        /// [1]: https://illumos.org/man/3C/door_revoke
        pub fn door_revoke(d: c_int) -> c_int;

        /// Report information about a door.
        ///
        /// See [`door_info(3C)`][1]. Pass
        /// [`DOOR_QUERY`](crate::DOOR_QUERY) to ask about the calling
        /// thread's own binding.
        ///
        /// [1]: https://illumos.org/man/3C/door_info
        pub fn door_info(d: c_int, info: *mut door_info_t) -> c_int;

        /// Invoke a door.
        ///
        /// See [`door_call(3C)`][1]. `params` is in/out: on return its
        /// fields describe where the results actually landed, which
        /// may be a fresh mapping rather than the supplied `rbuf`.
        ///
        /// [1]: https://illumos.org/man/3C/door_call
        pub fn door_call(d: c_int, params: *mut door_arg_t) -> c_int;

        /// Return from a door server procedure.
        ///
        /// See [`door_return(3C)`][1]. Private on purpose: the crate
        /// publishes the [`super::door_return`] wrapper instead,
        /// because this function only returns on failure.
        ///
        /// [1]: https://illumos.org/man/3C/door_return
        pub fn door_return(
            data_ptr: *mut c_char,
            data_size: size_t,
            desc_ptr: *mut door_desc_t,
            num_desc: c_uint,
        ) -> c_int;

        /// Report the client's credentials.
        ///
        /// See [`door_cred(3C)`][1]. Obsolete; prefer
        /// [`door_ucred`].
        ///
        /// [1]: https://illumos.org/man/3C/door_cred
        pub fn door_cred(info: *mut door_cred_t) -> c_int;

        /// Report the client's credentials as a `ucred_t`.
        ///
        /// See [`door_ucred(3C)`][1]. The caller owns the returned
        /// `ucred_t` and must free it with `ucred_free(3C)`. Passing
        /// a non-null `*info` reuses that allocation.
        ///
        /// [1]: https://illumos.org/man/3C/door_ucred
        pub fn door_ucred(info: *mut *mut libc::ucred_t) -> c_int;

        /// Bind the calling thread to a door's private thread pool.
        ///
        /// See [`door_bind(3C)`][1].
        ///
        /// [1]: https://illumos.org/man/3C/door_bind
        pub fn door_bind(did: c_int) -> c_int;

        /// Undo a [`door_bind`].
        ///
        /// See [`door_unbind(3C)`][1].
        ///
        /// [1]: https://illumos.org/man/3C/door_bind
        pub fn door_unbind() -> c_int;

        /// Read a door parameter.
        ///
        /// See [`door_getparam(3C)`][1]. `param` is one of
        /// [`DOOR_PARAM_DESC_MAX`](crate::DOOR_PARAM_DESC_MAX),
        /// [`DOOR_PARAM_DATA_MAX`](crate::DOOR_PARAM_DATA_MAX) or
        /// [`DOOR_PARAM_DATA_MIN`](crate::DOOR_PARAM_DATA_MIN).
        ///
        /// [1]: https://illumos.org/man/3C/door_getparam
        pub fn door_getparam(d: c_int, param: c_int, out: *mut size_t)
            -> c_int;

        /// Set a door parameter.
        ///
        /// See [`door_setparam(3C)`][1].
        ///
        /// [1]: https://illumos.org/man/3C/door_getparam
        pub fn door_setparam(d: c_int, param: c_int, val: size_t) -> c_int;

        /// Install a private door server thread creation function.
        ///
        /// See [`door_server_create(3C)`][1]. Returns the previous
        /// function, which may be the library default.
        ///
        /// [1]: https://illumos.org/man/3C/door_server_create
        pub fn door_server_create(
            create_proc: Option<door_server_func_t>,
        ) -> Option<door_server_func_t>;

        // --- <stropts.h> ---

        /// Attach a file descriptor to a path in the filesystem.
        ///
        /// See [`fattach(3C)`][1]. This is how a door becomes
        /// reachable by name.
        ///
        /// [1]: https://illumos.org/man/3C/fattach
        pub fn fattach(fildes: c_int, path: *const c_char) -> c_int;

        /// Undo an [`fattach`].
        ///
        /// See [`fdetach(3C)`][1].
        ///
        /// [1]: https://illumos.org/man/3C/fdetach
        pub fn fdetach(path: *const c_char) -> c_int;

        // --- <thread.h> ---

        /// Create a thread, Sun-style.
        ///
        /// See [`thr_create(3C)`][1]. On illumos this and
        /// `pthread_create` are the same underlying call; the crate
        /// uses this one only for
        /// [`THR_DAEMON`](crate::THR_DAEMON), which POSIX has no
        /// equivalent for.
        ///
        /// [1]: https://illumos.org/man/3C/thr_create
        pub fn thr_create(
            stk: *mut c_void,
            stksize: size_t,
            start_func: door_xcreate_thrfunc_t,
            arg: *mut c_void,
            flags: c_long,
            new_thread_id: *mut thread_t,
        ) -> c_int;

        /// Report the calling thread's stack bounds.
        ///
        /// See [`thr_stksegment(3C)`][1]. Used to validate a
        /// requested `DOOR_PARAM_DATA_MAX` against the stack the
        /// request will actually land on.
        ///
        /// [1]: https://illumos.org/man/3C/thr_stksegment
        pub fn thr_stksegment(ss: *mut libc::stack_t) -> c_int;

        /// The smallest usable thread stack size.
        ///
        /// See [`thr_min_stack(3C)`][1].
        ///
        /// [1]: https://illumos.org/man/3C/thr_min_stack
        pub fn thr_min_stack() -> size_t;

        // --- <pthread.h> ---

        /// Register handlers to run around `fork(2)`.
        ///
        /// See [`pthread_atfork(3C)`][1]. illumos has no `thr_atfork`;
        /// this is the only such facility.
        ///
        /// [1]: https://illumos.org/man/3C/pthread_atfork
        pub fn pthread_atfork(
            prepare: Option<extern "C" fn()>,
            parent: Option<extern "C" fn()>,
            child: Option<extern "C" fn()>,
        ) -> c_int;

        /// Enable or disable cancellation for the calling thread.
        ///
        /// See [`pthread_setcancelstate(3C)`][1].
        ///
        /// [1]: https://illumos.org/man/3C/pthread_setcancelstate
        pub fn pthread_setcancelstate(
            state: c_int,
            oldstate: *mut c_int,
        ) -> c_int;

        // --- <errno.h> ---

        /// illumos puts `errno` behind a per-thread accessor.
        pub fn ___errno() -> *mut c_int;
    }
}

// Re-export every binding directly. Their public signatures are the C
// ones; calling them is `unsafe` because they are `extern "C"`.
pub use raw::{
    door_bind, door_call, door_create, door_cred, door_getparam, door_info,
    door_revoke, door_server_create, door_setparam, door_ucred, door_unbind,
    door_xcreate, fattach, fdetach, pthread_atfork, pthread_setcancelstate,
    thr_create, thr_min_stack, thr_stksegment,
};

/// A non-zero errno.
///
/// Zero is unrepresentable, which is what makes [`door_return`]'s
/// signature honest: that function only ever returns on failure, so a
/// zero result would be a lie the type system can rule out.
pub type Errno = core::num::NonZeroI32;

/// Read `errno` for the calling thread.
///
/// illumos has no `errno` global; `<errno.h>` defines the name as
/// `(*___errno())` so each thread gets its own. This is the accessor
/// behind that macro.
#[inline]
pub fn errno() -> c_int {
    // SAFETY: ___errno() returns a valid pointer to this thread's
    // errno for as long as the thread lives.
    unsafe { *raw::___errno() }
}

/// Return from a door server procedure.
///
/// Returns *only* on failure. On success control never comes back, so
/// there is no success value. This wrapper reads errno and returns it,
/// never zero.
///
/// This is not `!`, because the function does return: `door_return(3C)`
/// documents `E2BIG`, `EMFILE`, `EFAULT` and `EINVAL`. It is not
/// `c_int` either, because zero is not a possible outcome.
///
/// # errno is not always set
///
/// Measured on OmniOS r151058, not assumed: when the kernel cannot
/// deliver a reply descriptor to the client, the raw `door_return`
/// returns `-1` and **leaves errno untouched**. See
/// `experiments/README.md`.
///
/// Since [`Errno`] cannot represent zero and this function must not
/// fabricate a success, it reports `EINVAL` on that path. Callers that
/// need to distinguish it should treat any `door_return` failure as
/// "the reply did not arrive" rather than branching on the value.
///
/// # Safety
///
/// `data_ptr` must point at `data_size` readable bytes and `desc_ptr`
/// at `num_desc` valid [`door_desc_t`]s, or both must be null with
/// their counts zero. The caller must be a door server thread.
///
/// Crucially, **on success this function never returns, so nothing on
/// the calling frame is ever dropped.** Callers must have released
/// every destructor-bearing value before calling it. See `GOALS.md`
/// §4.2.
#[inline]
pub unsafe fn door_return(
    data_ptr: *const c_char,
    data_size: size_t,
    desc_ptr: *const door_desc_t,
    num_desc: c_uint,
) -> Errno {
    raw::door_return(
        data_ptr as *mut c_char,
        data_size,
        desc_ptr as *mut door_desc_t,
        num_desc,
    );

    // Only reachable on failure. errno is usually set, but genuinely
    // is not on the undeliverable-descriptor path (see the doc comment
    // and experiments/README.md), so the fallback is load-bearing
    // rather than defensive.
    match Errno::new(errno()) {
        Some(e) => e,
        None => match Errno::new(libc::EINVAL) {
            Some(e) => e,
            None => unreachable!(),
        },
    }
}
