// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `#[door(handback)]`: a door that answers with a descriptor.
//!
//! `fork_and_descriptors.rs` shows the same trip when the server
//! procedure is written by hand, through `doors::__private`. This file
//! is the point of the shape: the same thing with no `__private`
//! anywhere, so a program that hands descriptors out is not pinned to
//! one patch release of the crate.
//!
//! Every test reads through the descriptor it gets back. A descriptor
//! that arrives is no proof on its own: the number would survive the
//! trip even if the file behind it had been closed.

use doors::{CallError, Client, Descriptors, Door, Request};
use std::io::{Read, Seek, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// Somewhere to hang a door, cleaned up even if the test fails.
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

/// Make a file with these contents and hand back the descriptor.
///
/// The path is unlinked straight away, so the descriptor is the only
/// thing keeping the file alive. If the crate closed it anywhere along
/// the way, the client would have nothing left to read.
fn tempfile_with(name: &str, contents: &[u8]) -> std::io::Result<OwnedFd> {
    let path = std::env::temp_dir()
        .join(format!("doors_handback_data_{}_{name}", std::process::id()));
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

/// Read everything behind a descriptor the door sent us.
///
/// The descriptor belongs to the `Reply`, which closes it on drop, so
/// this works on a copy of it.
fn read_all(fd: BorrowedFd<'_>) -> std::io::Result<String> {
    // SAFETY: `dup` on a descriptor that stays open for as long as
    // this borrow lives. The copy is a descriptor of our own, so the
    // `File` below closes ours and never the reply's.
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

// ---------------------------------------------------------------------

/// A door that lends files out.
struct Vault;

#[doors::server]
impl Vault {
    /// Answer with some bytes and one descriptor.
    ///
    /// `max_descriptors = 0` turns away descriptors the *caller* might
    /// send. It leaves this direction alone, which `refuse_desc` would
    /// not: that flag stops both, and then no client could read the
    /// descriptor below.
    #[door(handback, request_size = 0..=64, max_descriptors = 0)]
    fn lend(
        &self,
        req: Request<'_, Descriptors>,
    ) -> Result<(Vec<u8>, Vec<OwnedFd>), std::io::Error> {
        let fd = tempfile_with("lend", b"behind the descriptor")?;
        Ok((req.data().to_vec(), vec![fd]))
    }

    /// Two of them, because the reply carries a list and not one slot.
    #[door(handback, request_size = 0..=64, max_descriptors = 0)]
    fn lend_two(
        &self,
        _req: Request<'_, Descriptors>,
    ) -> Result<(Vec<u8>, Vec<OwnedFd>), std::io::Error> {
        let first = tempfile_with("first", b"the first one")?;
        let second = tempfile_with("second", b"the second one")?;
        Ok((b"two".to_vec(), vec![first, second]))
    }

    /// A failure. The error comes back and there is no descriptor.
    #[door(handback, request_size = 0..=64, max_descriptors = 0)]
    fn refuse(
        &self,
        _req: Request<'_, Descriptors>,
    ) -> Result<(Vec<u8>, Vec<OwnedFd>), std::io::Error> {
        Err(std::io::Error::other("the vault is shut"))
    }
}

/// A client that can receive descriptors.
///
/// `with_descriptors()` is what makes that possible. A plain client
/// closes whatever arrives and fails the call, so without this the
/// descriptor would never reach the test.
fn receiving_client(jamb: &DoorPath) -> Client<Descriptors> {
    Client::open(jamb.path())
        .expect("open")
        .with_descriptors()
        .expect("this door does not refuse descriptors")
}

/// The whole point of the shape: a descriptor comes back, and it is a
/// live one.
#[test]
fn a_handback_door_sends_a_usable_descriptor() {
    let jamb = DoorPath::new("one");

    let mut door = Door::builder(Vault)
        .thread_stack_size(256 * 1024)
        .build_lend()
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = receiving_client(&jamb);
    let reply = client.call(b"please").expect("call");
    assert_eq!(reply.data(), b"please", "the bytes come back too");

    let got = reply.descriptors();
    assert_eq!(got.len(), 1, "one descriptor was returned");

    let text = read_all(got[0].as_fd()).expect("read through it");
    assert_eq!(text, "behind the descriptor");
}

/// The reply carries a list, so a door can send more than one, in
/// order.
#[test]
fn a_handback_door_can_send_several() {
    let jamb = DoorPath::new("two");

    let mut door = Door::builder(Vault)
        .thread_stack_size(256 * 1024)
        .build_lend_two()
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = receiving_client(&jamb);
    let reply = client.call(b"").expect("call");
    assert_eq!(reply.data(), b"two");

    let got = reply.descriptors();
    assert_eq!(got.len(), 2, "both descriptors were returned");
    assert_eq!(read_all(got[0].as_fd()).expect("first"), "the first one");
    assert_eq!(read_all(got[1].as_fd()).expect("second"), "the second one");
}

/// An `Err` is still an error. The user's own message comes back, and
/// the reply has no descriptor in it.
#[test]
fn a_failing_handback_door_sends_no_descriptors() {
    let jamb = DoorPath::new("err");

    let mut door = Door::builder(Vault)
        .thread_stack_size(256 * 1024)
        .build_refuse()
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = receiving_client(&jamb);
    match client.call(b"") {
        Ok(reply) => panic!("expected an error, got {:?}", reply.data()),
        Err(CallError::Server { data }) => {
            assert!(
                String::from_utf8_lossy(&data).contains("the vault is shut"),
                "the user's own message should come back"
            );
        }
        other => panic!("expected CallError::Server, got {other:?}"),
    }
}

/// Finding 3, as a test. A client that never asked for descriptors
/// cannot read the one this door sends. The call fails instead of
/// handing back a reply with a hole in it.
#[test]
fn a_plain_client_cannot_receive_the_descriptor() {
    let jamb = DoorPath::new("plain");

    let mut door = Door::builder(Vault)
        .thread_stack_size(256 * 1024)
        .build_lend()
        .expect("build");
    door.attach(jamb.path())
        .unwrap_or_else(|e| panic!("attach {:?}: {e}", jamb.path()));

    let client = Client::open(jamb.path()).expect("open");
    match client.call(b"please") {
        Ok(_) => panic!("a Client<NoDescriptors> must not accept one"),
        Err(CallError::UnexpectedDescriptors { count }) => {
            assert_eq!(count, 1, "the door sent exactly one");
        }
        other => panic!("expected UnexpectedDescriptors, got {other:?}"),
    }
}
