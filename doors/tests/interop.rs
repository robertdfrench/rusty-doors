// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Talking to door peers that have never heard of this crate.
//!
//! `GOALS.md` invariant 13. Doors are an operating system facility
//! with existing users, so a binding that can only talk to itself is
//! not a binding. Both directions are checked here against real C
//! programs compiled at test time:
//!
//! - a Rust client calling a C door server, and
//! - a C client calling a Rust door.
//!
//! `untagged.rs` checks the same code paths without needing a C
//! compiler. This file is the one that would notice if the wire format
//! were subtly wrong.

use doors::__private::{run, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{Client, Door, Request};
use std::ffi::{c_char, c_uint, c_void};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

// ---------------------------------------------------------------------
// Building and running the C peers
// ---------------------------------------------------------------------

fn experiments_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is <repo>/doors.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("doors/ has a parent")
        .join("experiments")
}

/// Compile one of the C peers, returning the binary's path.
///
/// Returns `None` when there is no compiler, so a machine without gcc
/// skips these rather than reporting a failure it cannot act on.
fn compile(name: &str) -> Option<PathBuf> {
    let src = experiments_dir().join(format!("{name}.c"));
    // A different prefix from DoorPath: the two must never collide,
    // or creating the door path would delete the binary.
    let out = std::env::temp_dir().join(format!("doors_iop_bin_{name}"));

    let status = Command::new("gcc")
        .args(["-m64", "-Wall", "-o"])
        .arg(&out)
        .arg(&src)
        .status();

    match status {
        Ok(s) if s.success() => Some(out),
        Ok(s) => panic!("compiling {name}.c failed: {s}"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            eprintln!("skipping interop test: no gcc on this machine");
            None
        }
        Err(e) => panic!("running gcc: {e}"),
    }
}

/// A child process killed when the test ends, however it ends.
struct Kill(Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A door path removed when the test ends.
struct DoorPath(PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_iop_door_{name}"));
        let _ = std::fs::remove_file(&p);
        DoorPath(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    /// `fattach` needs the file to exist first.
    fn create(&self) -> &Self {
        std::fs::write(&self.0, b"").expect("create door path");
        self
    }
}

impl Drop for DoorPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---------------------------------------------------------------------
// Direction 1: a Rust client calls a C door
// ---------------------------------------------------------------------

/// The C server reverses the request. Nothing about its reply is this
/// crate's convention, so only the untagged client can read it.
#[test]
fn a_rust_client_calls_a_c_door() {
    let Some(bin) = compile("c_server") else {
        return;
    };
    let jamb = DoorPath::new("c_server");

    let mut child = Command::new(&bin)
        .arg(jamb.path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the C door server");

    // Wait for its "ready" line rather than sleeping, so the test is
    // not racing the door coming up.
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines.next().expect("server printed nothing");
    assert_eq!(ready.expect("read ready"), "ready");

    let _kill = Kill(child);

    let client = Client::open(jamb.path()).expect("open the C door");

    // The untagged reader gets the bytes the C server actually sent.
    let reply = client.untagged().call(b"abcdef").expect("untagged call");
    assert_eq!(
        reply.data(),
        b"fedcba",
        "a C door's reply must arrive verbatim"
    );

    // And the tagged reader does not, which is why untagged exists.
    // 'f' is 0x66, not a tag this crate ever writes.
    match client.call(b"abcdef") {
        Err(doors::CallError::Protocol(_)) => {}
        other => panic!("a tagged read of a C door should fail, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Direction 2: a C client calls a Rust door
// ---------------------------------------------------------------------

struct Upper;

/// SAFETY: registered with `door_create`; the kernel supplies every
/// argument.
unsafe extern "C" fn upper_untagged(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Upper, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        4096,
        ReplyProtocol::Untagged,
        |_s: &Upper, req: Request<'_, NoDescriptors>| {
            Outcome::bytes(Ok(req.data().to_ascii_uppercase()))
        },
    )
}

/// A C client cannot skip a status byte, so the Rust door must not
/// write one.
#[test]
fn a_c_client_calls_a_rust_door() {
    let Some(bin) = compile("c_client") else {
        return;
    };
    let jamb = DoorPath::new("rust_door");
    jamb.create();

    let mut door = Door::builder(Upper)
        .request_size(0..=4096)
        .thread_stack_size(256 * 1024)
        .untagged()
        .build(upper_untagged)
        .expect("build an untagged door");
    door.attach(jamb.path()).expect("attach");

    let out = Command::new(&bin)
        .arg(jamb.path())
        .arg("hello there")
        .output()
        .expect("run the C door client");

    assert!(
        out.status.success(),
        "C client failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout, b"HELLO THERE",
        "the C client must read the reply verbatim"
    );
}

/// The same Rust door in tagged mode is unreadable to the C client:
/// it sees the status byte as data. This is the failure the untagged
/// mode exists to prevent, pinned down so nobody removes it.
#[test]
fn a_tagged_rust_door_confuses_a_c_client() {
    let Some(bin) = compile("c_client") else {
        return;
    };
    let jamb = DoorPath::new("rust_door_tagged");
    jamb.create();

    /// SAFETY: as above, but tagged.
    unsafe extern "C" fn upper_tagged(
        cookie: *mut c_void,
        argp: *mut c_char,
        arg_size: usize,
        dp: *mut doors::__private::door_desc_t,
        n_desc: c_uint,
    ) {
        run::<Upper, NoDescriptors, _, io::Error>(
            cookie,
            argp,
            arg_size,
            dp,
            n_desc,
            4096,
            ReplyProtocol::Tagged,
            |_s: &Upper, req: Request<'_, NoDescriptors>| {
                Outcome::bytes(Ok(req.data().to_ascii_uppercase()))
            },
        )
    }

    let mut door = Door::builder(Upper)
        .request_size(0..=4096)
        .thread_stack_size(256 * 1024)
        .build(upper_tagged)
        .expect("build a tagged door");
    door.attach(jamb.path()).expect("attach");

    let out = Command::new(&bin)
        .arg(jamb.path())
        .arg("hi")
        .output()
        .expect("run the C door client");

    assert!(out.status.success());
    // A leading 0x00 status byte, then the payload. The C client has
    // no idea it is there.
    assert_eq!(
        out.stdout, b"\x00HI",
        "a tagged door sends a byte the C client cannot know about"
    );
}
