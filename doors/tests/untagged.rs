// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The untagged protocol, end to end (`GOALS.md` §6.5).
//!
//! An untagged door puts the server procedure's bytes on the wire and
//! nothing else — no status byte, no framing. That is what a door
//! written in C does, and it is what a client written in C expects to
//! read.
//!
//! `interop.rs` proves this against real C programs. This file proves
//! the same bytes travel over the same code paths, without needing a
//! compiler at test time, and — more usefully — it pins down what goes
//! wrong when the two sides disagree about the protocol.

use doors::__private::{run, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{Client, Door, Request};
use std::ffi::{c_char, c_uint, c_void};
use std::io;

struct Echo;

/// Replies with exactly the request bytes. No tag, no framing: the
/// protocol is chosen by which door registers this, below.
///
/// SAFETY: registered with `door_create`; the kernel supplies every
/// argument.
unsafe extern "C" fn echo_untagged(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Echo, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        64 * 1024,
        ReplyProtocol::Untagged,
        |_s: &Echo, req: Request<'_, NoDescriptors>| {
            Outcome::bytes(Ok(req.data().to_vec()))
        },
    )
}

struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_untagged_{name}"));
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

fn untagged_echo_door(name: &str) -> (Door<Echo>, DoorPath) {
    let jamb = DoorPath::new(name);
    let mut door = Door::builder(Echo)
        .request_size(0..=4096)
        .thread_stack_size(256 * 1024)
        .untagged()
        .build(echo_untagged)
        .expect("build untagged door");
    door.attach(jamb.path()).expect("attach");
    (door, jamb)
}

/// The reply is the bytes the procedure produced, and nothing else.
///
/// This is the property a C client depends on. One extra byte in front
/// and every offset it reads is wrong.
#[test]
fn an_untagged_reply_is_exactly_the_bytes() {
    let (_door, jamb) = untagged_echo_door("exact");
    let client = Client::open(jamb.path()).expect("open");

    for msg in [&b""[..], b"a", b"hello world", &[0u8; 1000][..]] {
        let reply = client.untagged().call(msg).expect("untagged call");
        assert_eq!(reply.data(), msg, "untagged reply must be verbatim");
    }
}

/// A tagged client reading an untagged door gets the wrong answer.
///
/// This is the whole reason the untagged mode has to exist. The tagged
/// reader takes the first byte as a status tag, so it either rejects a
/// perfectly good reply or hands back a payload one byte short. Either
/// way it is wrong, and this test says so out loud rather than leaving
/// it to be discovered against a real C server.
#[test]
fn a_tagged_client_misreads_an_untagged_door() {
    let (_door, jamb) = untagged_echo_door("mismatch");
    let client = Client::open(jamb.path()).expect("open");

    // 'h' is 0x68, which is not a tag this crate ever writes, so the
    // tagged reader rejects it outright.
    match client.call(b"hello") {
        Err(doors::CallError::Protocol(_)) => {}
        other => panic!("expected a protocol error, got {other:?}"),
    }

    // A payload starting with 0x00 is worse: it parses as tag 0, "Ok",
    // and the caller silently loses the first byte.
    let reply = client.call(&[0u8, b'x', b'y']).expect("parses as Ok");
    assert_eq!(
        reply.data(),
        b"xy",
        "the tagged reader eats the first byte, which is the bug \
         untagged mode exists to avoid"
    );

    // The untagged reader gets it right.
    let reply = client
        .untagged()
        .call(&[0u8, b'x', b'y'])
        .expect("untagged call");
    assert_eq!(reply.data(), &[0u8, b'x', b'y']);
}

/// `call_into` on the untagged path must not skip a byte either.
#[test]
fn untagged_call_into_keeps_the_first_byte() {
    let (_door, jamb) = untagged_echo_door("into");
    let client = Client::open(jamb.path()).expect("open");

    let mut buf = [0u8; 64];
    let got = client
        .untagged()
        .call_into(b"\x00abc", &mut buf)
        .expect("untagged call_into");
    assert_eq!(got, b"\x00abc");
}

/// An untagged door cannot report a server fault, so a panic comes
/// back as an empty reply rather than an error. Document by test.
#[test]
fn an_untagged_panic_is_an_empty_reply() {
    struct Boom;

    /// SAFETY: registered with `door_create`.
    unsafe extern "C" fn boom(
        cookie: *mut c_void,
        argp: *mut c_char,
        arg_size: usize,
        dp: *mut doors::__private::door_desc_t,
        n_desc: c_uint,
    ) {
        run::<Boom, NoDescriptors, _, io::Error>(
            cookie,
            argp,
            arg_size,
            dp,
            n_desc,
            4096,
            ReplyProtocol::Untagged,
            |_s: &Boom, _r: Request<'_, NoDescriptors>| -> Outcome<io::Error> {
                panic!("untagged panic");
            },
        )
    }

    let jamb = DoorPath::new("panic");
    let mut door = Door::builder(Boom)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .untagged()
        .build(boom)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    let reply = client.untagged().call(b"x").expect("call still answers");
    assert!(
        reply.data().is_empty(),
        "an untagged fault has no way to say so, so it says nothing"
    );

    // And the door still works, exactly as in the tagged case.
    let reply = client.untagged().call(b"y").expect("second call");
    assert!(reply.data().is_empty());
}
