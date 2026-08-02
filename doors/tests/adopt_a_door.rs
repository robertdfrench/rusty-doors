// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Calling a door you were handed.
//!
//! A door that travels between processes arrives as a descriptor, with
//! no path anywhere. `Client::open` cannot help. These tests cover the
//! three ways in: `Client::from_received` for a descriptor the kernel
//! delivered, `MaybeDoor` for one from anywhere else, and
//! `BorrowedClient` for one you must not take over.
//!
//! Every test that adopts a door then **calls** it. A descriptor that
//! arrives is no proof on its own: the number would survive the trip
//! even if nothing were behind it.

use doors::{
    BorrowedClient, CallError, Client, Descriptors, Door, MaybeDoor,
    NoDescriptors, NotADoorReason, Request, SentFd,
};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::sync::Arc;

/// Somewhere to hang a door, cleaned up even if the test fails.
struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_adopt_{name}"));
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

/// Make a file with these contents and hand back the descriptor.
///
/// The path is unlinked straight away, so the descriptor is the only
/// thing keeping the file alive. Reading through it later proves the
/// crate did not close it.
fn tempfile_with(name: &str, contents: &[u8]) -> std::io::Result<OwnedFd> {
    let path = std::env::temp_dir()
        .join(format!("doors_adopt_data_{}_{name}", std::process::id()));
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

/// Read everything behind a descriptor, without taking it over.
fn read_all(fd: BorrowedFd<'_>) -> std::io::Result<String> {
    // SAFETY: `dup` on a descriptor that stays open for as long as
    // this borrow lives. The copy is ours, so the `File` below closes
    // ours and never the caller's.
    let copy = unsafe { libc::dup(fd.as_raw_fd()) };
    if copy < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `dup` just produced this descriptor and nothing else
    // holds it, so the `File` may own it.
    let mut f = unsafe { std::fs::File::from_raw_fd(copy) };
    let mut text = String::new();
    f.rewind()?;
    f.read_to_string(&mut text)?;
    Ok(text)
}

/// An owned descriptor for a door we serve ourselves.
///
/// `as_sendable` lends the door's descriptor and never gives it up,
/// which is right for sending but leaves nothing to own. `dup` makes a
/// second descriptor for the same door, and that one is ours.
fn dup_of<S>(door: &Door<S>) -> std::io::Result<OwnedFd>
where
    S: Send + Sync + 'static,
{
    let lent = door.as_sendable()?;
    let SentFd::Shared(borrowed) = lent else {
        unreachable!("as_sendable lends; it never gives the door away")
    };
    // SAFETY: `borrowed` is live for as long as `lent`, which is still
    // in scope here.
    let copy = unsafe { libc::dup(borrowed.as_raw_fd()) };
    if copy < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `dup` just produced this descriptor and nothing else
    // holds it.
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}

// ---------------------------------------------------------------------
// The doors
// ---------------------------------------------------------------------

/// The door that gets handed around. It is never attached to a path.
struct Inner;

#[doors::server]
impl Inner {
    /// `max_descriptors = 0` turns away descriptors a caller might
    /// send. It is not `refuse_desc`, so this door does **not** carry
    /// `DOOR_REFUSE_DESC` and a client may still opt in to them.
    #[door(procedure, request_size = 0..=64, max_descriptors = 0)]
    fn answer(
        &self,
        req: Request<'_, Descriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        let mut out = b"the door we were handed answered: ".to_vec();
        out.extend_from_slice(req.data());
        Ok(out)
    }
}

/// A door created with `DOOR_REFUSE_DESC`.
struct Strict;

#[doors::server]
impl Strict {
    #[door(procedure, refuse_desc, request_size = 0..=64)]
    fn answer(
        &self,
        _req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        Ok(b"the strict door answered".to_vec())
    }
}

/// A door that hands other descriptors out.
struct Lender {
    inner: Arc<Door<Inner>>,
}

#[doors::server]
impl Lender {
    /// Reply with a descriptor for the door in `inner`.
    #[door(handback, request_size = 0..=64, max_descriptors = 0)]
    fn lend(
        &self,
        _req: Request<'_, Descriptors>,
    ) -> Result<(Vec<u8>, Vec<OwnedFd>), std::io::Error> {
        Ok((b"take it".to_vec(), vec![dup_of(&self.inner)?]))
    }

    /// Reply with a descriptor that is not a door at all.
    #[door(handback, request_size = 0..=64, max_descriptors = 0)]
    fn lend_a_file(
        &self,
        _req: Request<'_, Descriptors>,
    ) -> Result<(Vec<u8>, Vec<OwnedFd>), std::io::Error> {
        let fd = tempfile_with("lent", b"an ordinary file")?;
        Ok((b"not a door".to_vec(), vec![fd]))
    }
}

fn build_inner() -> Door<Inner> {
    Door::builder(Inner)
        .thread_stack_size(256 * 1024)
        .build_answer()
        .expect("build the inner door")
}

fn build_strict() -> Door<Strict> {
    Door::builder(Strict)
        .thread_stack_size(256 * 1024)
        .build_answer()
        .expect("build the strict door")
}

/// A client for the lender door, able to receive what it sends.
fn lender_client(jamb: &DoorPath) -> Client<Descriptors> {
    Client::open(jamb.path())
        .expect("open the lender")
        .with_descriptors()
        .expect("the lender does not refuse descriptors")
}

// ---------------------------------------------------------------------
// Client::from_received
// ---------------------------------------------------------------------

/// The finding, as a test. A door arrives in a reply, is adopted with
/// no path anywhere, and answers.
#[test]
fn a_door_that_arrived_can_be_adopted_and_called() {
    let jamb = DoorPath::new("lend");
    let inner = Arc::new(build_inner());

    let mut lender = Door::builder(Lender {
        inner: Arc::clone(&inner),
    })
    .thread_stack_size(256 * 1024)
    .build_lend()
    .expect("build the lender");
    lender
        .attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let reply = lender_client(&jamb).call(b"").expect("call the lender");
    assert_eq!(reply.data(), b"take it");

    let arrived = reply
        .into_descriptors()
        .pop()
        .expect("the lender sent one descriptor");
    assert!(arrived.attributes().is_descriptor());
    assert!(
        arrived.door_id().is_some(),
        "asking the kernel gives a door id for a real door"
    );

    // No path was ever involved for the inner door. This is the step
    // that used to need doors-sys and a hand-built door_arg_t.
    let adopted = Client::from_received(arrived).expect("adopt the door");

    let answer = adopted.call(b"ping").expect("call the adopted door");
    assert_eq!(answer.data(), b"the door we were handed answered: ping");

    // And again, because a Client is worth keeping.
    let again = adopted.call(b"pong").expect("call it a second time");
    assert_eq!(again.data(), b"the door we were handed answered: pong");
}

/// A descriptor that is not a door must not become a `Client`, and the
/// caller must not lose it.
///
/// This test also pins down **why** `from_received` makes a
/// `door_info(3C)` call instead of reading the attributes the kernel
/// delivered. An ordinary file arrives with `DOOR_DESCRIPTOR` set,
/// exactly as a door does. That bit means "a descriptor is being
/// passed", not "this is a door", and there is nothing else to read.
#[test]
fn from_received_hands_back_a_descriptor_that_is_not_a_door() {
    let jamb = DoorPath::new("file");
    let inner = Arc::new(build_inner());

    let mut lender = Door::builder(Lender { inner })
        .thread_stack_size(256 * 1024)
        .build_lend_a_file()
        .expect("build the lender");
    lender
        .attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let reply = lender_client(&jamb).call(b"").expect("call the lender");
    let arrived = reply.into_descriptors().pop().expect("one descriptor");

    // The measurement. An ordinary file carries the same bit a door
    // does, so no bit test could have told them apart.
    assert!(
        arrived.attributes().is_descriptor(),
        "DOOR_DESCRIPTOR is set on a plain file too"
    );
    assert!(
        !arrived.attributes().is_local() && !arrived.attributes().is_revoked(),
        "and no other attribute is set either"
    );
    assert!(
        arrived.door_id().is_none(),
        "asking the kernel is the only thing that gets this right"
    );

    let err = match Client::from_received(arrived) {
        Ok(_) => panic!("a plain file must not become a Client"),
        Err(e) => e,
    };
    assert!(
        matches!(err.reason, NotADoorReason::NotADoor(_)),
        "expected NotADoor, got {:?}",
        err.reason
    );

    // The whole point: we still have the descriptor, and it works.
    assert_eq!(
        read_all(err.fd.as_fd()).expect("read through the returned fd"),
        "an ordinary file"
    );
}

// ---------------------------------------------------------------------
// MaybeDoor
// ---------------------------------------------------------------------

/// A door reached only by a descriptor, checked and then called.
#[test]
fn probably_accepts_a_real_door() {
    let inner = build_inner();
    let copy = dup_of(&inner).expect("dup the door's descriptor");

    let client = MaybeDoor::new(copy).into_client().expect("it is a door");
    let reply = client.call(b"hello").expect("call it");
    assert_eq!(reply.data(), b"the door we were handed answered: hello");
}

/// `inspect` answers without giving the descriptor up.
#[test]
fn probably_can_be_inspected_first() {
    let inner = build_inner();
    let maybe = MaybeDoor::new(dup_of(&inner).expect("dup"));

    let info = maybe.inspect().expect("door_info answers for a door");
    assert!(info.is_local(), "we serve this door ourselves");
    assert!(!info.is_revoked());
    assert!(!info.refuses_descriptors());
    assert_eq!(
        info.target_pid(),
        std::process::id() as libc::pid_t,
        "we serve it, so the target is this process"
    );

    // Still ours to convert afterwards.
    let client = maybe.into_client().expect("still a door");
    assert_eq!(
        client.call(b"hi").expect("call").data(),
        b"the door we were handed answered: hi"
    );
}

/// A descriptor that is not a door is refused, and handed back intact.
#[test]
fn probably_hands_back_a_descriptor_that_is_not_a_door() {
    let file = tempfile_with("plain", b"still readable").expect("make a file");

    let err = match MaybeDoor::new(file).into_client() {
        Ok(_) => panic!("a plain file must not become a Client"),
        Err(e) => e,
    };

    match err.reason {
        NotADoorReason::NotADoor(errno) => assert_eq!(
            errno.get(),
            libc::EBADF,
            "door_info reports EBADF for a non-door"
        ),
        other => panic!("expected NotADoor, got {other:?}"),
    }

    // Handing the descriptor back is worth nothing if it is dead.
    assert_eq!(
        read_all(err.fd.as_fd()).expect("read through the returned fd"),
        "still readable"
    );
}

/// A revoked door is refused now rather than on the first call.
#[test]
fn probably_refuses_a_revoked_door() {
    let door = build_inner();
    let copy = dup_of(&door).expect("dup the door's descriptor");

    // Dropping a Door revokes it. Our copy of the descriptor survives
    // and now names a door that answers nothing.
    drop(door);

    let err = match MaybeDoor::new(copy).into_client() {
        Ok(_) => panic!("a revoked door must not become a Client"),
        Err(e) => e,
    };
    assert_eq!(
        err.reason,
        NotADoorReason::Revoked,
        "the kernel still reports the door, marked revoked"
    );
}

/// `DOOR_REFUSE_DESC` stops the descriptor-carrying conversion, and
/// only that one. The door is still fine for plain calls.
#[test]
fn probably_refuses_descriptors_only_when_the_door_does() {
    let strict = build_strict();

    let plain = MaybeDoor::new(dup_of(&strict).expect("dup"))
        .into_client()
        .expect("a refuse_desc door is still a perfectly good door");
    assert_eq!(
        plain.call(b"").expect("call it").data(),
        b"the strict door answered"
    );

    let err = match MaybeDoor::new(dup_of(&strict).expect("dup"))
        .into_client_with_descriptors()
    {
        Ok(_) => panic!("a DOOR_REFUSE_DESC door cannot carry descriptors"),
        Err(e) => e,
    };
    assert_eq!(err.reason, NotADoorReason::RefusesDescriptors);

    // Refused, so the descriptor is still ours, and still a door.
    MaybeDoor::new(err.fd)
        .into_client()
        .expect("the returned descriptor is unharmed");
}

/// A door that does not refuse descriptors converts either way.
#[test]
fn probably_makes_a_descriptor_carrying_client() {
    let inner = build_inner();
    let client = MaybeDoor::new(dup_of(&inner).expect("dup"))
        .into_client_with_descriptors()
        .expect("this door does not refuse descriptors");

    let reply = client
        .call_with_descriptors(b"empty", Vec::new())
        .expect("call with no descriptors is still a call");
    assert_eq!(reply.data(), b"the door we were handed answered: empty");
}

/// The descriptor goes in and comes out again, untouched.
#[test]
fn probably_gives_the_descriptor_back() {
    let file = tempfile_with("roundtrip", b"round trip").expect("make a file");
    let raw = file.as_raw_fd();

    let back = MaybeDoor::new(file).into_fd();

    assert_eq!(back.as_raw_fd(), raw, "the same descriptor came back");
    assert_eq!(
        read_all(back.as_fd()).expect("read through it"),
        "round trip",
        "and it is still open"
    );
}

// ---------------------------------------------------------------------
// BorrowedClient
// ---------------------------------------------------------------------

/// Call the same door many times without owning it.
#[test]
fn a_borrowed_client_never_closes_what_it_borrowed() {
    let inner = build_inner();
    let held = dup_of(&inner).expect("dup the door's descriptor");

    {
        let door = BorrowedClient::new(held.as_fd()).expect("it is a door");
        for i in 0..8u8 {
            let reply = door.call(&[b'a' + i]).expect("call");
            let mut want = b"the door we were handed answered: ".to_vec();
            want.push(b'a' + i);
            assert_eq!(reply.data(), &want[..]);
        }
        // Every Client method is here, through Deref.
        assert!(door.info().expect("info").is_local());
        assert!(door.limits().expect("limits").data_max >= 64);
    }

    // The borrow is over. If it had closed our descriptor, this would
    // fail — or worse, succeed against whatever took the number.
    let again =
        BorrowedClient::new(held.as_fd()).expect("our descriptor is untouched");
    assert_eq!(
        again.call(b"z").expect("call").data(),
        b"the door we were handed answered: z"
    );
}

/// A borrowed client refuses a descriptor that is not a door. There is
/// nothing to hand back, so the error is the reason on its own.
#[test]
fn a_borrowed_client_refuses_a_non_door() {
    let file = tempfile_with("borrow", b"not a door").expect("make a file");

    match BorrowedClient::new(file.as_fd()) {
        Ok(_) => panic!("a plain file is not a door"),
        Err(NotADoorReason::NotADoor(errno)) => {
            assert_eq!(errno.get(), libc::EBADF)
        }
        Err(other) => panic!("expected NotADoor, got {other:?}"),
    }

    // We never gave the descriptor up, so of course we still have it.
    assert_eq!(
        read_all(file.as_fd()).expect("read through it"),
        "not a door"
    );
}

/// The descriptor-carrying borrow refuses a `DOOR_REFUSE_DESC` door.
#[test]
fn a_borrowed_client_with_descriptors_checks_the_door() {
    let strict = build_strict();
    let held = dup_of(&strict).expect("dup");

    match BorrowedClient::with_descriptors(held.as_fd()) {
        Ok(_) => panic!("this door refuses descriptors"),
        Err(reason) => {
            assert_eq!(reason, NotADoorReason::RefusesDescriptors)
        }
    }

    // The plain borrow is still fine.
    let plain = BorrowedClient::new(held.as_fd()).expect("still a door");
    assert_eq!(
        plain.call(b"").expect("call").data(),
        b"the strict door answered"
    );
}

// ---------------------------------------------------------------------
// The unchecked constructors
// ---------------------------------------------------------------------

/// Skipping the check works when the promise is kept.
#[test]
fn an_unchecked_client_calls_a_real_door() {
    let inner = build_inner();

    let plain = Client::<NoDescriptors>::from_fd_unchecked(
        dup_of(&inner).expect("dup"),
    );
    assert_eq!(
        plain.call(b"p").expect("call").data(),
        b"the door we were handed answered: p"
    );

    let carrying =
        Client::<Descriptors>::from_fd_unchecked(dup_of(&inner).expect("dup"));
    assert_eq!(
        carrying
            .call_with_descriptors(b"q", Vec::new())
            .expect("call")
            .data(),
        b"the door we were handed answered: q"
    );
}

/// Breaking the promise gives a wrong answer, not broken memory.
///
/// This is the measurement behind the claim in
/// `Client::from_fd_unchecked`'s documentation: a descriptor that is
/// not a door simply makes every call fail. That is why the method is
/// named `_unchecked` and not marked `unsafe`.
#[test]
fn an_unchecked_client_over_a_non_door_only_fails() {
    let file = tempfile_with("unchecked", b"not a door").expect("make a file");
    let client = Client::<NoDescriptors>::from_fd_unchecked(file);

    match client.call(b"anything") {
        Ok(reply) => panic!("a plain file cannot answer: {:?}", reply.data()),
        Err(CallError::Rejected { returned, errno }) => {
            assert_eq!(errno.get(), libc::EBADF, "door_call says EBADF");
            assert!(returned.is_empty(), "a plain call sends no descriptors");
        }
        Err(other) => panic!("expected Rejected with EBADF, got {other:?}"),
    }

    // Twice, to show nothing was damaged by the first attempt.
    assert!(client.call(b"anything").is_err());
}

/// What skipping the `DOOR_REFUSE_DESC` check actually costs.
///
/// The call fails, and the descriptors come back. `door_call` reports
/// `ENOTSUP`, and the kernel rejects a descriptor-carrying call to
/// such a door *before* taking anything — so `ENOTSUP` is on the
/// hand-back list beside `EFAULT` and `EBADF`, and
/// `CallError::Rejected` carries the descriptors home.
///
/// It was not always so. This path used to fall through to
/// `Consumed`, which says the kernel took them, and the descriptors
/// were dropped without being closed: one leak per call. What settled
/// it was `experiments/refuse_desc_errno.c`, which compares
/// `st_dev`/`st_ino`/`st_rdev` rather than trusting `F_GETFD` — a
/// descriptor number can be closed and reissued, so `F_GETFD`
/// succeeding proves nothing on its own.
///
/// So the cost of skipping the check is now a failed call, not a
/// leaked descriptor. That is still an argument for the one
/// `door_info` call that `MaybeDoor::into_client_with_descriptors`
/// makes, just a smaller one.
#[test]
fn skipping_the_refuse_desc_check_returns_the_descriptors() {
    let strict = build_strict();
    let client =
        Client::<Descriptors>::from_fd_unchecked(dup_of(&strict).expect("dup"));

    // A plain call is fine. Only descriptors are refused.
    assert_eq!(
        client.call(b"").expect("plain calls still work").data(),
        b"the strict door answered"
    );

    let victim = tempfile_with("victim", b"returned").expect("make a file");

    match client.call_with_descriptors(b"", vec![SentFd::released(victim)]) {
        Ok(_) => panic!("a DOOR_REFUSE_DESC door must refuse this"),
        Err(CallError::Rejected { returned, errno }) => {
            assert_eq!(
                errno.get(),
                libc::ENOTSUP,
                "door_call reports ENOTSUP for DOOR_REFUSE_DESC"
            );
            assert_eq!(returned.len(), 1, "the descriptor must come back");

            // And it must be the real thing, not a number that now
            // names something else. Read it.
            let mut f = std::fs::File::from(
                returned.into_iter().next().expect("one descriptor"),
            );
            let mut text = String::new();
            let _ = f.rewind();
            f.read_to_string(&mut text).expect("read the returned fd");
            assert_eq!(text, "returned", "the returned fd is our file");
        }
        Err(other) => panic!("expected Rejected(ENOTSUP), got {other:?}"),
    }
}

/// A borrowed client can skip the check too.
#[test]
fn an_unchecked_borrowed_client_calls_a_real_door() {
    let inner = build_inner();
    let held = dup_of(&inner).expect("dup");

    let door = BorrowedClient::new_unchecked(held.as_fd());
    assert_eq!(
        door.call(b"u").expect("call").data(),
        b"the door we were handed answered: u"
    );

    let carrying = BorrowedClient::with_descriptors_unchecked(held.as_fd());
    assert_eq!(
        carrying
            .call_with_descriptors(b"v", Vec::new())
            .expect("call")
            .data(),
        b"the door we were handed answered: v"
    );
}
