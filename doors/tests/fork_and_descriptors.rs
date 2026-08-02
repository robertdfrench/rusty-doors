// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `GOALS.md` §9.2: fork behaviour and descriptor passing.
//!
//! These are the tests that would have caught the two mistakes the C
//! interface makes easiest: a forked child tearing down the door its
//! parent is still serving, and a descriptor being closed twice or not
//! at all.

use doors::__private::{run, Descriptors, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{CallError, Client, Door, ForkResult, ReceivedFd, Request, SentFd};
use std::ffi::{c_char, c_uint, c_void};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsFd, FromRawFd, OwnedFd};

struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_fd_{name}"));
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
// fork
// ---------------------------------------------------------------------

struct Marker;

/// SAFETY: registered with `door_create`.
unsafe extern "C" fn marker_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Marker, NoDescriptors, _, std::io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        4096,
        ReplyProtocol::Tagged,
        |_s: &Marker, _r: Request<'_, NoDescriptors>| {
            Outcome::bytes(Ok(b"alive".to_vec()))
        },
    )
}

/// A child that drops its inherited `Door` must not take the parent's
/// door with it. `GOALS.md` §12.8.
///
/// This is the failure the C interface invites: the descriptor is
/// inherited, so `door_revoke` and `fdetach` in the child look
/// perfectly reasonable and quietly break the parent.
#[test]
fn a_forked_child_does_not_tear_down_the_parent_door() {
    let jamb = DoorPath::new("fork_teardown");

    let mut door = Door::builder(Marker)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .build(marker_proc)
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    // It works before the fork.
    let client = Client::open(jamb.path()).expect("open");
    assert_eq!(client.call(b"x").expect("before fork").data(), b"alive");

    match doors::fork().expect("fork") {
        ForkResult::Child => {
            // The atfork child handler has already disowned this door.
            // Dropping it must revoke nothing and unlink nothing.
            drop(door);
            // Do not run the test harness's exit path in the child.
            unsafe { libc::_exit(0) };
        }
        ForkResult::Parent { child } => {
            let mut status: libc::c_int = 0;
            // SAFETY: child is the pid fork() just returned.
            unsafe { libc::waitpid(child, &mut status, 0) };

            // The path must still exist and still answer.
            assert!(
                jamb.path().exists(),
                "the child unlinked the parent's door path"
            );
            let reply = client.call(b"x").expect("after the child exited");
            assert_eq!(reply.data(), b"alive");
        }
    }
}

/// A child must not be able to revoke the parent's door either.
#[test]
fn a_forked_child_cannot_revoke_or_detach() {
    let jamb = DoorPath::new("fork_revoke");

    let mut door = Door::builder(Marker)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .build(marker_proc)
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    match doors::fork().expect("fork") {
        ForkResult::Child => {
            // Both must refuse, rather than damaging the parent.
            let detached = door.detach(jamb.path()).is_err();
            let revoked = door.revoke().is_err();
            // SAFETY: report through the exit status; the child must
            // not run the harness's own teardown.
            let code = if detached && revoked { 0 } else { 1 };
            unsafe { libc::_exit(code) };
        }
        ForkResult::Parent { child } => {
            let mut status: libc::c_int = 0;
            // SAFETY: child is the pid fork() just returned.
            unsafe { libc::waitpid(child, &mut status, 0) };
            let code = (status >> 8) & 0xff;
            assert_eq!(
                code, 0,
                "the child was allowed to detach or revoke the parent's door"
            );

            let client = Client::open(jamb.path()).expect("open");
            assert_eq!(
                client.call(b"x").expect("parent still works").data(),
                b"alive"
            );
        }
    }
}

// ---------------------------------------------------------------------
// Descriptors
// ---------------------------------------------------------------------

struct Reader;

/// Reads whatever the caller sent a descriptor for, and sends back a
/// fresh descriptor of its own.
///
/// SAFETY: registered with `door_create`.
unsafe extern "C" fn reader_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Reader, Descriptors, _, std::io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        64 * 1024,
        ReplyProtocol::Tagged,
        |_s: &Reader, req: Request<'_, Descriptors>| {
            let fds: &[ReceivedFd] = req.descriptors();
            if fds.is_empty() {
                return Outcome::bytes(Err(std::io::Error::other(
                    "expected one descriptor",
                )));
            }

            // Read the file the caller handed us.
            let borrowed = fds[0].as_fd();
            // SAFETY: we dup so the File we build does not close the
            // ReceivedFd's descriptor, which ReceivedFd owns.
            let dup = unsafe {
                libc::dup(std::os::fd::AsRawFd::as_raw_fd(&borrowed))
            };
            if dup < 0 {
                return Outcome::bytes(Err(std::io::Error::last_os_error()));
            }
            // SAFETY: dup just gave us this descriptor.
            let mut f = unsafe { std::fs::File::from_raw_fd(dup) };
            let mut text = String::new();
            let _ = f.rewind();
            if let Err(e) = f.read_to_string(&mut text) {
                return Outcome::bytes(Err(e));
            }

            // Send one back, too.
            let tmp = match tempfile_with(b"from the server") {
                Ok(t) => t,
                Err(e) => return Outcome::bytes(Err(e)),
            };

            Outcome {
                data: Ok(text.into_bytes()),
                descriptors: vec![tmp],
            }
        },
    )
}

/// Make a temporary file, write to it, and hand back the descriptor.
fn tempfile_with(contents: &[u8]) -> std::io::Result<OwnedFd> {
    // A fresh name every time. This used to be keyed on the pid
    // alone, which meant every test in the process shared one path:
    // two tests running at once truncated and rewrote each other's
    // file, and the loser read the winner's bytes. It failed about
    // once in ten runs and looked like a descriptor bug rather than
    // what it was.
    static NEXT: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir()
        .join(format!("doors_tmp_{}_{n}", std::process::id()));
    let mut f = std::fs::File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    f.write_all(contents)?;
    f.rewind()?;
    // Unlink now; the descriptor keeps it alive.
    let _ = std::fs::remove_file(&path);
    Ok(OwnedFd::from(f))
}

#[test]
fn descriptors_travel_both_ways() {
    let jamb = DoorPath::new("descriptors");

    let mut door = Door::builder(Reader)
        .request_size(0..=64)
        .max_descriptors(4)
        .thread_stack_size(256 * 1024)
        .build(reader_proc)
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = Client::open(jamb.path())
        .expect("open")
        .with_descriptors()
        .expect("this door accepts descriptors");

    let payload = tempfile_with(b"from the client").expect("tempfile");
    let reply = client
        .call_with_descriptors(b"", vec![SentFd::Released(payload)])
        .expect("call_with_descriptors");

    // The server read our file.
    assert_eq!(reply.data(), b"from the client");

    // And sent one of its own back.
    let got = reply.descriptors();
    assert_eq!(got.len(), 1, "expected one descriptor back");

    // SAFETY: dup so the File does not close the ReceivedFd's copy.
    let dup =
        unsafe { libc::dup(std::os::fd::AsRawFd::as_raw_fd(&got[0].as_fd())) };
    assert!(dup >= 0);
    // SAFETY: dup just produced this descriptor.
    let mut f = unsafe { std::fs::File::from_raw_fd(dup) };
    let mut text = String::new();
    let _ = f.rewind();
    f.read_to_string(&mut text).expect("read the server's file");
    assert_eq!(text, "from the server");
}

/// A `Shared` descriptor stays ours. Sending one must not close it.
#[test]
fn a_shared_descriptor_is_still_usable_afterwards() {
    let jamb = DoorPath::new("shared");

    let mut door = Door::builder(Reader)
        .request_size(0..=64)
        .max_descriptors(4)
        .thread_stack_size(256 * 1024)
        .build(reader_proc)
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = Client::open(jamb.path())
        .expect("open")
        .with_descriptors()
        .expect("accepts descriptors");

    let mine = tempfile_with(b"still mine").expect("tempfile");

    let reply = client
        .call_with_descriptors(b"", vec![SentFd::Shared(mine.as_fd())])
        .expect("call");
    assert_eq!(reply.data(), b"still mine");
    drop(reply);

    // We kept ownership, so this must still work. If the crate had
    // closed it, the read would fail with EBADF.
    // SAFETY: dup of a descriptor we still own.
    let dup =
        unsafe { libc::dup(std::os::fd::AsRawFd::as_raw_fd(&mine.as_fd())) };
    assert!(dup >= 0, "the shared descriptor was closed by the crate");
    // SAFETY: dup just produced this descriptor.
    drop(unsafe { OwnedFd::from_raw_fd(dup) });
}

/// A door that refuses descriptors must refuse to produce a
/// descriptor-capable client at all, rather than failing later.
#[test]
fn with_descriptors_refuses_a_door_that_refuses_them() {
    let jamb = DoorPath::new("refuse_desc");

    let mut door = Door::builder(Marker)
        .request_size(0..=64)
        .refuse_descriptors()
        .thread_stack_size(256 * 1024)
        .build(marker_proc)
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = Client::open(jamb.path()).expect("open");
    assert!(
        client.with_descriptors().is_err(),
        "a DOOR_REFUSE_DESC door must not yield a Client<Descriptors>"
    );
}

/// The kernel refuses the descriptor before the server ever runs, and
/// the caller gets its `Released` descriptor back rather than a
/// silently closed one.
#[test]
fn sending_to_a_refusing_door_reports_an_error() {
    let jamb = DoorPath::new("refused_send");

    // A door that accepts descriptors, so with_descriptors() succeeds,
    // but whose parameter says zero are allowed.
    let mut door = Door::builder(Reader)
        .request_size(0..=64)
        .max_descriptors(0)
        .thread_stack_size(256 * 1024)
        .build(reader_proc)
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = Client::open(jamb.path())
        .expect("open")
        .with_descriptors()
        .expect("accepts descriptors");

    let fd = tempfile_with(b"nope").expect("tempfile");
    match client.call_with_descriptors(b"", vec![SentFd::Released(fd)]) {
        Ok(_) => panic!("a door with DESC_MAX=0 should not accept one"),
        Err(CallError::Rejected { returned, .. }) => {
            assert_eq!(returned.len(), 1, "the descriptor should come back");
        }
        Err(CallError::Consumed(_)) => {
            // Also acceptable: the kernel took it. What must NOT happen
            // is the caller being left holding a closed OwnedFd.
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
