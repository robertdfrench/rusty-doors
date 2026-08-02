// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! End to end: a real door, a real client, a real call.
//!
//! Everything else in the test suite checks one piece. These tests
//! check that the pieces fit: `door_xcreate` with our own server
//! threads, the trampoline, the §3.9 status tag, cookie resolution,
//! and [`Reply`] unmapping what the kernel gave it.
//!
//! The server procedures here are written by hand, the way a user
//! would before reaching for `#[doors::server]`.

use doors::__private::{run, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{CallError, Client, Door, Request, ServerFault};
use std::ffi::{c_char, c_uint, c_void};
use std::io;

/// A place to hang a door for the duration of one test.
///
/// `fattach` needs the path to exist first, and a door left attached
/// would outlive the test, so this cleans up on `Drop` even when the
/// test fails.
struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_rt_{name}"));
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
// A door that echoes with a prefix
// ---------------------------------------------------------------------

struct Echo {
    prefix: &'static str,
}

/// SAFETY: registered with `door_xcreate`; the kernel supplies every
/// argument.
unsafe extern "C" fn echo_proc(
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
        ReplyProtocol::Tagged,
        |state: &Echo, req: Request<'_, NoDescriptors>| {
            let mut out = state.prefix.as_bytes().to_vec();
            out.extend_from_slice(req.data());
            Outcome::bytes(Ok(out))
        },
    )
}

#[test]
fn call_a_door_and_get_the_reply_back() {
    let jamb = DoorPath::new("echo");

    let mut door = Door::builder(Echo { prefix: "echo:" })
        .request_size(0..=4096)
        .max_descriptors(0)
        .thread_stack_size(256 * 1024)
        .build(echo_proc)
        .expect("build the door");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    let reply = client.call(b"hello").expect("call");

    assert_eq!(reply.data(), b"echo:hello");
}

#[test]
fn many_calls_on_one_door() {
    let jamb = DoorPath::new("echo_many");

    let mut door = Door::builder(Echo { prefix: "n:" })
        .request_size(0..=4096)
        .thread_stack_size(256 * 1024)
        .build(echo_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    for i in 0..64u32 {
        let msg = i.to_string();
        let reply = client.call(msg.as_bytes()).expect("call");
        assert_eq!(reply.data(), format!("n:{i}").as_bytes());
    }
}

#[test]
fn the_limits_the_door_declared_are_readable() {
    let jamb = DoorPath::new("limits");

    let mut door = Door::builder(Echo { prefix: "" })
        .request_size(8..=4096)
        .max_descriptors(3)
        .thread_stack_size(256 * 1024)
        .build(echo_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    let limits = client.limits().expect("limits");
    assert_eq!(limits.data_min, 8);
    assert_eq!(limits.data_max, 4096);
    assert_eq!(limits.desc_max, 3);
}

// ---------------------------------------------------------------------
// A door whose procedure panics
// ---------------------------------------------------------------------

struct Panicky;

/// SAFETY: as above.
unsafe extern "C" fn panicky_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Panicky, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        64 * 1024,
        ReplyProtocol::Tagged,
        |_state: &Panicky, req: Request<'_, NoDescriptors>| {
            if req.data() == b"boom" {
                panic!("the server procedure panicked on purpose");
            }
            Outcome::bytes(Ok(b"fine".to_vec()))
        },
    )
}

/// A panic must not unwind across the `extern "C"` boundary
/// (`GOALS.md` §12.4), must reach the client as a §3.9 tag 2, and must
/// leave the door usable.
#[test]
fn a_panicking_procedure_becomes_an_error_and_the_door_survives() {
    let jamb = DoorPath::new("panic");

    let mut door = Door::builder(Panicky)
        .request_size(0..=4096)
        .thread_stack_size(256 * 1024)
        .build(panicky_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");

    // It works before.
    assert_eq!(client.call(b"ok").expect("first call").data(), b"fine");

    // It reports the panic as a fault, not as a crash.
    match client.call(b"boom") {
        Err(CallError::ServerFailed(ServerFault::Panicked)) => {}
        other => panic!("expected ServerFailed(Panicked), got {other:?}"),
    }

    // And it still works afterwards. If the panic had unwound through
    // the trampoline, the server thread would be gone by now.
    assert_eq!(client.call(b"ok").expect("third call").data(), b"fine");
}

// ---------------------------------------------------------------------
// A door that returns an error
// ---------------------------------------------------------------------

struct Failing;

/// SAFETY: as above.
unsafe extern "C" fn failing_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Failing, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        64 * 1024,
        ReplyProtocol::Tagged,
        |_state: &Failing, _req: Request<'_, NoDescriptors>| {
            Outcome::bytes(Err(io::Error::other("no thank you")))
        },
    )
}

#[test]
fn a_user_error_arrives_as_call_error_server() {
    let jamb = DoorPath::new("failing");

    let mut door = Door::builder(Failing)
        .request_size(0..=4096)
        .thread_stack_size(256 * 1024)
        .build(failing_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    match client.call(b"anything") {
        Err(CallError::Server { data }) => {
            let text = String::from_utf8_lossy(&data);
            assert!(text.contains("no thank you"), "error text was {text:?}");
        }
        other => panic!("expected CallError::Server, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Big replies: the mapping path
// ---------------------------------------------------------------------

struct Bulk;

/// SAFETY: as above.
unsafe extern "C" fn bulk_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Bulk, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        // Well above the inline size, so this exercises the spill.
        1024 * 1024,
        ReplyProtocol::Tagged,
        |_state: &Bulk, req: Request<'_, NoDescriptors>| {
            let n: usize =
                String::from_utf8_lossy(req.data()).parse().unwrap_or(0);
            Outcome::bytes(Ok(vec![b'x'; n]))
        },
    )
}

/// A reply larger than [`ReplyBuf`]'s inline array still arrives
/// whole, and the mapping the kernel makes for it is unmapped when the
/// [`Reply`] drops.
///
/// [`ReplyBuf`]: doors::ReplyBuf
/// [`Reply`]: doors::Reply
#[test]
fn a_big_reply_survives_the_spill_and_the_mapping() {
    let jamb = DoorPath::new("bulk");

    let mut door = Door::builder(Bulk)
        .request_size(0..=64)
        .thread_stack_size(512 * 1024)
        .build(bulk_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");

    for size in [1usize, 100, 2047, 2048, 2049, 8192, 200_000] {
        let reply = client
            .call(size.to_string().as_bytes())
            .unwrap_or_else(|e| panic!("call for {size} failed: {e:?}"));
        assert_eq!(reply.data().len(), size, "wrong length for {size}");
        assert!(reply.data().iter().all(|b| *b == b'x'));
    }
}

/// Repeat the mapping path enough times that a leaked mapping would
/// show up as address space exhaustion.
#[test]
fn replies_do_not_leak_their_mappings() {
    let jamb = DoorPath::new("noleak");

    let mut door = Door::builder(Bulk)
        .request_size(0..=64)
        .thread_stack_size(512 * 1024)
        .build(bulk_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");

    // 512 * 1 MiB is far more address space than a 64-bit process
    // would miss, but a leak of the *mapping count* still shows up as
    // a failure to map.
    for _ in 0..512 {
        let reply = client.call(b"1000000").expect("bulk call");
        assert_eq!(reply.data().len(), 1_000_000);
        drop(reply);
    }
}

// ---------------------------------------------------------------------
// call_into: the no-mapping path
// ---------------------------------------------------------------------

#[test]
fn call_into_uses_the_callers_buffer_and_reports_overflow() {
    let jamb = DoorPath::new("into");

    let mut door = Door::builder(Bulk)
        .request_size(0..=64)
        .thread_stack_size(512 * 1024)
        .build(bulk_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");

    // Fits: the reply lands in our buffer and no mapping is made.
    let mut buf = [0u8; 256];
    let got = client.call_into(b"100", &mut buf).expect("small call_into");
    assert_eq!(got.len(), 100);
    assert!(got.iter().all(|b| *b == b'x'));

    // Does not fit: we get told how much was needed, and no mapping is
    // handed back for us to forget about.
    let mut small = [0u8; 8];
    match client.call_into(b"100000", &mut small) {
        Err(CallError::ReplyTooBig { needed }) => {
            assert!(needed >= 100_000, "needed was {needed}");
        }
        other => panic!("expected ReplyTooBig, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Revoke
// ---------------------------------------------------------------------

#[test]
fn revoke_hands_the_state_back_and_stops_the_door() {
    let jamb = DoorPath::new("revoke");

    let mut door = Door::builder(Echo { prefix: "r:" })
        .request_size(0..=4096)
        .thread_stack_size(256 * 1024)
        .build(echo_proc)
        .expect("build");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    assert_eq!(client.call(b"x").expect("before").data(), b"r:x");

    let state = door.revoke().expect("revoke");
    assert_eq!(state.prefix, "r:");

    // The door is gone; the call must fail rather than hang or
    // succeed against freed state.
    assert!(client.call(b"x").is_err(), "call after revoke should fail");
}
