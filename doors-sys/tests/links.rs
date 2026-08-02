// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Prove the bindings resolve at link time and behave like the C ones.
//!
//! The layout tests in `src/layout.rs` never call anything, so they
//! would still pass if every symbol here were misspelled. These tests
//! call the real functions.

use doors_sys::*;

/// `door_getparam` on a descriptor that is not a door must fail with
/// `EBADF`. This is the cheapest proof that we are calling the real
/// `door_getparam` and reading the real errno.
#[test]
fn door_getparam_rejects_a_non_door() {
    let mut out: libc::size_t = 0;
    // fd 0 is stdin, a valid descriptor that is definitely not a door.
    let rc = unsafe { door_getparam(0, DOOR_PARAM_DATA_MAX, &mut out) };
    assert_eq!(rc, -1, "door_getparam on stdin should fail");
    assert_eq!(errno(), libc::EBADF, "expected EBADF from a non-door fd");
}

/// `door_info` on a non-door fails the same way.
#[test]
fn door_info_rejects_a_non_door() {
    let mut info: door_info_t = unsafe { core::mem::zeroed() };
    let rc = unsafe { door_info(0, &mut info) };
    assert_eq!(rc, -1);
    assert_eq!(errno(), libc::EBADF);
}

/// `door_unbind` from a thread that is not bound fails with `EBADF`.
///
/// Measured, not assumed: `door_unbind(3C)` documents EBADF for "the
/// thread is not currently bound to a door", and the kernel agrees.
#[test]
fn door_unbind_without_a_binding() {
    let rc = unsafe { door_unbind() };
    assert_eq!(rc, -1);
    assert_eq!(errno(), libc::EBADF);
}

/// `door_revoke` on a non-door fails with `EBADF`.
#[test]
fn door_revoke_rejects_a_non_door() {
    let rc = unsafe { door_revoke(0) };
    assert_eq!(rc, -1);
    assert_eq!(errno(), libc::EBADF);
}

/// `thr_min_stack` returns something plausible. This links
/// `<thread.h>`.
#[test]
fn thr_min_stack_is_sane() {
    let n = unsafe { thr_min_stack() };
    assert!(n > 0, "thr_min_stack returned {n}");
    assert!(
        n < 1 << 20,
        "thr_min_stack returned {n}, suspiciously large"
    );
}

/// `thr_stksegment` describes the current thread's stack, and the
/// address of a local lives inside it. This also proves our
/// `libc::stack_t` is the right shape.
#[test]
fn thr_stksegment_brackets_the_stack() {
    let mut ss: libc::stack_t = unsafe { core::mem::zeroed() };
    let rc = unsafe { thr_stksegment(&mut ss) };
    assert_eq!(rc, 0, "thr_stksegment failed, errno {}", errno());
    assert!(ss.ss_size > 0);

    // ss_sp is the *high* end of the stack on illumos; the stack grows
    // down from it. A local variable must sit in [sp - size, sp).
    let local = 0u8;
    let addr = &local as *const u8 as usize;
    let high = ss.ss_sp as usize;
    let low = high - ss.ss_size;
    assert!(
        (low..high).contains(&addr),
        "local at {addr:#x} outside stack [{low:#x}, {high:#x})"
    );
}

/// `fdetach` on a path nothing is attached to fails. This links
/// `<stropts.h>`.
#[test]
fn fdetach_rejects_an_unattached_path() {
    let path = c"/dev/null";
    let rc = unsafe { fdetach(path.as_ptr()) };
    assert_eq!(rc, -1);
    // EINVAL: not an attached path. Some paths give EPERM instead;
    // either way it must not claim success.
    assert!(
        matches!(errno(), libc::EINVAL | libc::EPERM),
        "errno {}",
        errno()
    );
}

/// `pthread_setcancelstate` is what every door server thread calls
/// first. Check it works and reports the previous state.
#[test]
fn setcancelstate_round_trips() {
    let mut old: core::ffi::c_int = -1;
    let rc =
        unsafe { pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &mut old) };
    assert_eq!(rc, 0);
    assert_eq!(old, PTHREAD_CANCEL_ENABLE, "threads start cancellable");

    let mut old2: core::ffi::c_int = -1;
    let rc =
        unsafe { pthread_setcancelstate(PTHREAD_CANCEL_ENABLE, &mut old2) };
    assert_eq!(rc, 0);
    assert_eq!(old2, PTHREAD_CANCEL_DISABLE, "we just disabled it");
}

/// The kernel agrees with our `door_desc_t` size. `door_call` copies
/// `desc_num * sizeof(door_desc_t)` bytes out of `desc_ptr`, so if our
/// size were wrong this would read the wrong memory. Here we only need
/// the arithmetic to be visible to the compiler.
#[test]
fn door_desc_t_array_stride() {
    let a: [door_desc_t; 3] = unsafe { core::mem::zeroed() };
    let base = a.as_ptr() as usize;
    let second = &a[1] as *const door_desc_t as usize;
    assert_eq!(second - base, 24, "door_desc_t stride must be 24");
}
