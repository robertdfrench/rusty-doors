// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Sending your own door to somebody else.
//!
//! Handing a door to another program means handing over a file
//! descriptor, so you need one. `Door::as_sendable` lends it.
//!
//! The door in the first test is never attached to a path. That is the
//! part that used to be impossible: the only way to get a descriptor
//! for your own door was to `open(2)` the path you had `fattach`ed it
//! to, so a door with no path could not be sent at all — even though
//! the kernel is perfectly happy to pass it.

use doors::__private::{run, Descriptors, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{Client, Door, ForkResult, Request};
use std::ffi::{c_char, c_uint, c_void};
use std::io;
use std::os::fd::{AsRawFd, RawFd};

/// A file for a door to be attached to, removed when the test ends.
struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_send_{name}"));
        let _ = std::fs::remove_file(&p);
        std::fs::write(&p, b"").expect("create door path");
        DoorPath(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for DoorPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------
// The door that gets sent
// ---------------------------------------------------------------------

struct Inner;

/// The door we hand over. It replies untagged, so whoever calls it
/// gets exactly these bytes and the relay below can pass them straight
/// on without two status bytes to unpick.
///
/// SAFETY: registered with `door_create`.
unsafe extern "C" fn inner_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Inner, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        4096,
        ReplyProtocol::Untagged,
        |_s: &Inner, _r: Request<'_, NoDescriptors>| {
            Outcome::bytes(Ok(b"the inner door answered".to_vec()))
        },
    )
}

// ---------------------------------------------------------------------
// The door we send it to
// ---------------------------------------------------------------------

struct Relay;

/// Takes the door it is handed and calls it, then replies with what
/// that door said.
///
/// This is what makes the test conclusive. A descriptor that arrives
/// and can be called is a working door; anything less would only show
/// that some number crossed the wire.
///
/// SAFETY: registered with `door_create`.
unsafe extern "C" fn relay_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Relay, Descriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        4096,
        ReplyProtocol::Tagged,
        |_s: &Relay, req: Request<'_, Descriptors>| {
            let Some(door) = req.descriptors().first() else {
                return Outcome::bytes(Err(io::Error::other(
                    "no descriptor arrived",
                )));
            };
            Outcome::bytes(call_by_descriptor(door.as_raw_fd()))
        },
    )
}

/// Call a door given only its descriptor.
///
/// [`Client`] opens a door by path, and the door under test has no
/// path, so this call is made by hand. It is small on purpose: the
/// reply is a couple of dozen bytes and the result buffer is much
/// bigger, so the kernel copies into that buffer and maps nothing.
/// The check below proves that rather than assuming it.
fn call_by_descriptor(fd: RawFd) -> io::Result<Vec<u8>> {
    let mut buf = [0u8; 256];
    let ours = buf.as_mut_ptr().cast::<c_char>();

    let mut arg = doors_sys::door_arg_t {
        data_ptr: std::ptr::null_mut(),
        data_size: 0,
        desc_ptr: std::ptr::null_mut(),
        desc_num: 0,
        rbuf: ours,
        rsize: buf.len(),
    };

    // SAFETY: `fd` is the descriptor the kernel just delivered to this
    // server procedure, and `arg` stays alive and in place for the
    // whole call.
    let rc = unsafe { doors_sys::door_call(fd, &mut arg) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }

    if arg.rbuf != ours {
        // The kernel mapped a fresh area instead of using ours. Unmap
        // it, or every call leaks one mapping on this thread.
        //
        // SAFETY: rbuf and rsize describe the mapping the kernel just
        // made, and nothing else refers to it.
        unsafe { libc::munmap(arg.rbuf.cast(), arg.rsize) };
        return Err(io::Error::other("the reply did not fit the buffer"));
    }

    // SAFETY: the kernel filled in data_ptr and data_size, and they
    // describe bytes inside `buf`, which is still alive here.
    let data = unsafe {
        std::slice::from_raw_parts(arg.data_ptr as *const u8, arg.data_size)
    };
    Ok(data.to_vec())
}

// ---------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------

/// A door with no path can be sent, the peer can call it, and the
/// sender still has it afterwards.
#[test]
fn a_door_with_no_path_can_be_sent_and_called() {
    let jamb = DoorPath::new("relay");

    // Note there is no `attach` for this one anywhere in the test.
    let inner = Door::builder(Inner)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .build(inner_proc)
        .expect("build the inner door");

    let mut relay = Door::builder(Relay)
        .request_size(0..=64)
        .max_descriptors(1)
        .thread_stack_size(256 * 1024)
        .build(relay_proc)
        .expect("build the relay door");
    relay
        .attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = Client::open(jamb.path())
        .expect("open the relay")
        .with_descriptors()
        .expect("the relay accepts descriptors");

    let lent = inner
        .as_sendable()
        .expect("a live door lends its descriptor");
    let reply = client
        .call_with_descriptors(b"", vec![lent])
        .expect("call the relay");

    // The relay called the door it was handed, so what travelled was a
    // door that really works.
    assert_eq!(reply.data(), b"the inner door answered");
    drop(reply);

    // `Shared` must not close what it borrowed. If the crate had
    // closed our descriptor, asking the kernel about it would fail.
    inner
        .info()
        .expect("our own door is still open after sending it");

    // And it can be sent again. A descriptor that had been consumed
    // could not be.
    let again = client
        .call_with_descriptors(
            b"",
            vec![inner.as_sendable().expect("lend it a second time")],
        )
        .expect("second call");
    assert_eq!(again.data(), b"the inner door answered");
}

/// A door a `fork` disowned must refuse to be lent.
///
/// The child's copy of the descriptor is already closed, so the number
/// is free and may name some other file by now. Lending it would hand
/// a peer that file. This is why `as_sendable` returns a `Result`.
#[test]
fn a_forked_child_cannot_lend_the_parent_door() {
    let jamb = DoorPath::new("fork");

    let mut door = Door::builder(Inner)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .build(inner_proc)
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    match doors::fork().expect("fork") {
        ForkResult::Child => {
            let refused = door.as_sendable().is_err();
            // SAFETY: the answer goes back as an exit status. The
            // child must not run the test harness's own teardown.
            unsafe { libc::_exit(if refused { 0 } else { 1 }) };
        }
        ForkResult::Parent { child } => {
            let mut status: libc::c_int = 0;
            // SAFETY: child is the pid fork() just returned.
            unsafe { libc::waitpid(child, &mut status, 0) };
            assert_eq!(
                (status >> 8) & 0xff,
                0,
                "the child was allowed to lend the parent's door"
            );

            // The parent never lost it.
            let _ = door.as_sendable().expect("the owner may still lend it");
        }
    }
}
