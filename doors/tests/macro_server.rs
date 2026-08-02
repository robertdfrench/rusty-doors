// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `#[doors::server]`, used the way the crate docs say to use it.
//!
//! `roundtrip.rs` proves the machinery works when a server procedure
//! is written by hand. This proves the macro produces the same thing
//! without the user seeing any of it.

use doors::{CallError, Client, Door, NoDescriptors, Request, ServerFault};

/// Somewhere to hang a door, cleaned up even if the test fails.
struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_macro_{name}"));
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

/// Server state. Every invocation gets a `&Greeter`.
struct Greeter {
    greeting: String,
}

#[doors::server]
impl Greeter {
    /// The default shape: a `Request` in, bytes out.
    #[door(refuse_desc)]
    fn hello(
        &self,
        req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        let who = String::from_utf8_lossy(req.data()).to_string();
        Ok(format!("{}, {who}!", self.greeting).into_bytes())
    }

    /// A second door on the same state, to prove one `impl` block can
    /// carry more than one.
    #[door(refuse_desc)]
    fn shout(
        &self,
        req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        Ok(String::from_utf8_lossy(req.data())
            .to_uppercase()
            .into_bytes())
    }

    /// Returns an error, to prove the §3.9 tag 1 path is wired up.
    #[door(refuse_desc)]
    fn refuse(
        &self,
        _req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        Err(std::io::Error::other("not today"))
    }

    /// Panics, to prove the trampoline still catches it.
    #[door(refuse_desc)]
    fn explode(
        &self,
        _req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, std::io::Error> {
        panic!("this panic must not escape the extern \"C\" frame");
    }
}

#[test]
fn a_generated_door_answers() {
    let jamb = DoorPath::new("hello");

    let mut door = Door::builder(Greeter {
        greeting: String::from("hello"),
    })
    .thread_stack_size(256 * 1024)
    .build_hello()
    .expect("build_hello");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    let reply = client.call(b"world").expect("call");
    assert_eq!(reply.data(), b"hello, world!");
}

#[test]
fn two_doors_from_one_impl_block() {
    let jamb = DoorPath::new("shout");

    let mut door = Door::builder(Greeter {
        greeting: String::from("unused"),
    })
    .thread_stack_size(256 * 1024)
    .build_shout()
    .expect("build_shout");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    let reply = client.call(b"quiet").expect("call");
    assert_eq!(reply.data(), b"QUIET");
}

#[test]
fn a_generated_door_reports_a_user_error() {
    let jamb = DoorPath::new("refuse");

    let mut door = Door::builder(Greeter {
        greeting: String::new(),
    })
    .thread_stack_size(256 * 1024)
    .build_refuse()
    .expect("build_refuse");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    match client.call(b"please") {
        Err(CallError::Server { data }) => {
            assert!(String::from_utf8_lossy(&data).contains("not today"));
        }
        other => panic!("expected CallError::Server, got {other:?}"),
    }
}

#[test]
fn a_generated_door_survives_a_panic() {
    let jamb = DoorPath::new("explode");

    let mut door = Door::builder(Greeter {
        greeting: String::new(),
    })
    .thread_stack_size(256 * 1024)
    .build_explode()
    .expect("build_explode");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    match client.call(b"boom") {
        Err(CallError::ServerFailed(ServerFault::Panicked)) => {}
        other => panic!("expected ServerFailed(Panicked), got {other:?}"),
    }

    // The door must still be there afterwards.
    match client.call(b"again") {
        Err(CallError::ServerFailed(ServerFault::Panicked)) => {}
        other => panic!("door stopped working after a panic: {other:?}"),
    }
}

/// The state really is shared: the door sees what we built it with.
#[test]
fn the_cookie_carries_the_state() {
    let jamb = DoorPath::new("state");

    let mut door = Door::builder(Greeter {
        greeting: String::from("guten tag"),
    })
    .thread_stack_size(256 * 1024)
    .build_hello()
    .expect("build_hello");
    door.attach(jamb.path()).expect("attach");

    let client = Client::open(jamb.path()).expect("open");
    let reply = client.call(b"welt").expect("call");
    assert_eq!(reply.data(), b"guten tag, welt!");

    // And revoke gives it back.
    let state = door.revoke().expect("revoke");
    assert_eq!(state.greeting, "guten tag");
}
