// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Refusing incoming descriptors while still replying with one.
//!
//! `DOOR_REFUSE_DESC` sounds like it is only about what a caller may
//! send. It is not: one flag governs both directions. A door that sets
//! it can never reply with a descriptor, because
//! [`Client::with_descriptors`] refuses such a door, and that method is
//! also the only way a client can receive one.
//!
//! `max_descriptors(0)` is the way to get what people usually want. The
//! kernel still rejects a call that carries a descriptor, before the
//! server procedure runs, and the reply direction keeps working.
//!
//! These tests pin that combination, because it is what a real user
//! needed and had to find the hard way.
//!
//! [`Client::with_descriptors`]: doors::Client::with_descriptors

use doors::__private::{run, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{CallError, Client, Door, Request, SentFd};
use std::ffi::{c_char, c_uint, c_void};
use std::io::{Read, Seek, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_handback_{name}"));
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

/// Make a temporary file with these contents and hand back the
/// descriptor. The file is unlinked at once; the descriptor keeps it
/// alive, so the reader on the other side sees the bytes.
///
/// The name carries a counter because the tests in this file run at the
/// same time in one process, and two of them making the same path would
/// truncate each other's file.
fn tempfile_with(name: &str, contents: &[u8]) -> std::io::Result<OwnedFd> {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "doors_handback_tmp_{}_{name}_{n}",
        std::process::id()
    ));
    let mut f = std::fs::File::options()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    f.write_all(contents)?;
    f.rewind()?;
    let _ = std::fs::remove_file(&path);
    Ok(OwnedFd::from(f))
}

/// Read a descriptor's whole contents without taking it over.
fn read_all(fd: std::os::fd::BorrowedFd<'_>) -> String {
    // SAFETY: dup so the File we build closes its own copy and leaves
    // the borrowed descriptor alone.
    let dup = unsafe { libc::dup(std::os::fd::AsRawFd::as_raw_fd(&fd)) };
    assert!(dup >= 0, "dup failed: {}", std::io::Error::last_os_error());
    // SAFETY: dup just produced this descriptor and nothing else owns
    // it.
    let mut f = unsafe { std::fs::File::from_raw_fd(dup) };
    let mut s = String::new();
    let _ = f.rewind();
    f.read_to_string(&mut s).expect("read the descriptor");
    s
}

/// The door's state. `calls` counts how many times the server
/// procedure actually ran; the test holds the other end of the `Arc`,
/// because a `Door` does not lend its state out.
struct Handback {
    calls: Arc<AtomicUsize>,
}

/// Takes no descriptor and always returns one.
///
/// This is the shape the finding is about: the door does not want
/// descriptors on the way in, so the request policy is
/// [`NoDescriptors`], but every reply carries one.
///
/// SAFETY: registered with `door_create`.
unsafe extern "C" fn handback_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Handback, NoDescriptors, _, std::io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        4096,
        ReplyProtocol::Tagged,
        |s: &Handback, _r: Request<'_, NoDescriptors>| {
            s.calls.fetch_add(1, Ordering::SeqCst);
            match tempfile_with("server", b"handed back") {
                Ok(fd) => Outcome {
                    data: Ok(b"ok".to_vec()),
                    descriptors: vec![fd],
                },
                Err(e) => Outcome::bytes(Err(e)),
            }
        },
    )
}

/// Build the handback door and hand back the call counter with it.
fn build_handback(jamb: &DoorPath) -> (Door<Handback>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut door = Door::builder(Handback {
        calls: Arc::clone(&calls),
    })
    .request_size(0..=64)
    // The workaround this whole file is about. NOT refuse_descriptors:
    // that would block the reply direction as well.
    .max_descriptors(0)
    .thread_stack_size(256 * 1024)
    .build(handback_proc)
    .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));
    (door, calls)
}

/// The workaround works: `max_descriptors(0)` still lets a client be a
/// `Client<Descriptors>` and read the descriptor out of the reply.
#[test]
fn max_descriptors_zero_still_hands_a_descriptor_back() {
    let jamb = DoorPath::new("handback");
    let (_door, _calls) = build_handback(&jamb);

    // The door does not carry DOOR_REFUSE_DESC, so this succeeds.
    // With refuse_descriptors() it would fail here, and the reply
    // below would be unreachable.
    let client = Client::open(jamb.path())
        .expect("open")
        .with_descriptors()
        .expect("max_descriptors(0) must not refuse a descriptor client");

    let reply = client.call(b"go").expect("call");
    assert_eq!(reply.data(), b"ok");

    let got = reply.descriptors();
    assert_eq!(got.len(), 1, "the reply should carry one descriptor");
    assert_eq!(read_all(got[0].as_fd()), "handed back");
}

/// The other half of the intent: the kernel turns away a call that
/// carries a descriptor, and it does so before the server procedure
/// runs.
#[test]
fn max_descriptors_zero_still_rejects_a_sent_descriptor() {
    let jamb = DoorPath::new("reject");
    let (_door, calls) = build_handback(&jamb);

    let client = Client::open(jamb.path())
        .expect("open")
        .with_descriptors()
        .expect("max_descriptors(0) must not refuse a descriptor client");

    // One good call first, so the counter below is comparing against a
    // door that is known to be answering.
    let reply = client.call(b"go").expect("first call");
    assert_eq!(reply.descriptors().len(), 1);
    drop(reply);
    let before = calls.load(Ordering::SeqCst);
    assert_eq!(before, 1, "the server procedure should have run once");

    let payload = tempfile_with("client", b"should not arrive").expect("tmp");
    match client.call_with_descriptors(b"go", vec![SentFd::Released(payload)]) {
        Ok(_) => panic!("a door with DESC_MAX=0 must not accept a descriptor"),
        // The call never left, so our Released descriptor comes back.
        Err(CallError::Rejected { returned, .. }) => {
            assert_eq!(returned.len(), 1, "the descriptor should come back");
        }
        // Also allowed: the kernel took the descriptor. What must never
        // happen is the caller keeping an OwnedFd that is already
        // closed.
        Err(CallError::Consumed(_)) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }

    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "the server procedure ran; the descriptor should have been \
         refused before it"
    );

    // And the door still works afterwards.
    let reply = client.call(b"go").expect("call after the rejected one");
    assert_eq!(reply.descriptors().len(), 1);
}

/// Why the workaround is needed at all: the same handback door built
/// with `refuse_descriptors()` cannot give anybody a client that could
/// read its reply.
#[test]
fn refuse_descriptors_makes_the_handback_unreachable() {
    let jamb = DoorPath::new("refused");

    let mut door = Door::builder(Handback {
        calls: Arc::new(AtomicUsize::new(0)),
    })
    .request_size(0..=64)
    .refuse_descriptors()
    .thread_stack_size(256 * 1024)
    .build(handback_proc)
    .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = Client::open(jamb.path()).expect("open");
    assert!(
        client.with_descriptors().is_err(),
        "DOOR_REFUSE_DESC blocks the reply direction too, so this door \
         can never hand its descriptor to anyone"
    );
}
