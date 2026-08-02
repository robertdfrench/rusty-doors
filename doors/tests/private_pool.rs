// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A door with its own pool of server threads must actually be served.
//!
//! This is the regression test for the first finding in
//! `DOORS-CRATE-FINDINGS.md`. A door built with `private_pool()`
//! attached, opened and answered a call or two, and then stopped
//! answering for ever. A real consumer measured 2 calls served out of
//! 1000 attempts, with nothing logged on either side.
//!
//! The cause: `DOOR_PRIVATE` gives a door its own pool, and a thread
//! joins that pool by calling `door_bind(3C)` with the door's
//! descriptor *before* it parks in `door_return`. A thread that parks
//! without binding joins the process-wide pool instead, where a
//! private door never sees it. `experiments/private_pool.c` shows the
//! difference in C: without the bind the same program hangs.
//!
//! # Why every call is on a watchdog
//!
//! The failure is a hang, not a wrong answer. A test that simply made
//! the calls would, on a regression, block for ever and take the whole
//! suite with it — which is nearly as unhelpful as the bug. So each
//! round runs on its own thread and the result is collected with a
//! deadline. A regression fails; it does not wedge.
//!
//! # Why concurrency
//!
//! One call at a time does not reproduce it. The reporter saw the
//! first couple of calls succeed, because the thread that happened to
//! be bound when the door was created served them. It is the calls
//! that need a *second* server thread that never come back.

use doors::__private::{run, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{Client, Door, Request};
use std::ffi::{c_char, c_uint, c_void};
use std::io;
use std::sync::mpsc;
use std::time::Duration;

/// How long the whole batch of calls may take before we call it a
/// hang. Generous: on an idle guest these finish in milliseconds.
const DEADLINE: Duration = Duration::from_secs(20);

/// How many calls to make at once. Has to be more than one, or the
/// bug hides: a single caller is served by whichever thread was
/// already bound.
const CALLERS: usize = 16;

struct Counter;

/// SAFETY: registered with `door_create`; the kernel supplies every
/// argument.
unsafe extern "C" fn count_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Counter, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        4096,
        ReplyProtocol::Tagged,
        |_s: &Counter, req: Request<'_, NoDescriptors>| {
            // Take a moment, so several calls really are in flight at
            // once and the door needs more than one server thread.
            std::thread::sleep(Duration::from_millis(5));
            Outcome::bytes(Ok(req.data().to_vec()))
        },
    )
}

struct DoorPath(std::path::PathBuf);

impl DoorPath {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("doors_pp_{name}"));
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

/// Make `CALLERS` calls at once and report how many came back.
///
/// Runs the batch on its own thread so a hang shows up as a missing
/// result rather than a wedged test.
fn hammer(path: &std::path::Path) -> Option<usize> {
    let path = path.to_path_buf();
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        let mut threads = Vec::new();
        for i in 0..CALLERS {
            let p = path.clone();
            threads.push(std::thread::spawn(move || {
                let client = Client::open(&p).ok()?;
                let msg = format!("call{i}");
                let reply = client.call(msg.as_bytes()).ok()?;
                (reply.data() == msg.as_bytes()).then_some(())
            }));
        }
        let ok = threads
            .into_iter()
            .filter_map(|t| t.join().ok().flatten())
            .count();
        let _ = tx.send(ok);
    });

    rx.recv_timeout(DEADLINE).ok()
}

/// The headline case: a private pool must serve every caller.
#[test]
fn a_private_pool_door_serves_concurrent_calls() {
    let jamb = DoorPath::new("private");

    let mut door = Door::builder(Counter)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .private_pool()
        .build(count_proc)
        .expect("build a private-pool door");
    door.attach(jamb.path()).expect("attach");

    match hammer(jamb.path()) {
        None => panic!(
            "a private-pool door stopped answering: {CALLERS} calls did \
             not finish within {DEADLINE:?}. Server threads are most \
             likely parking without door_bind, so they joined the \
             process-wide pool instead of this door's own."
        ),
        Some(n) => assert_eq!(
            n, CALLERS,
            "only {n} of {CALLERS} calls to a private-pool door returned"
        ),
    }
}

/// The shared pool must keep working, which is the risk in the fix: a
/// bound thread serves only its own door, so binding a shared door
/// would take it out of the general pool and starve everything else.
#[test]
fn a_shared_pool_door_still_serves_concurrent_calls() {
    let jamb = DoorPath::new("shared");

    let mut door = Door::builder(Counter)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .build(count_proc)
        .expect("build a shared-pool door");
    door.attach(jamb.path()).expect("attach");

    match hammer(jamb.path()) {
        None => panic!("a shared-pool door stopped answering"),
        Some(n) => assert_eq!(n, CALLERS),
    }
}

/// Both kinds in one process. This is where binding the wrong door
/// would show up: the private door takes threads out of the global
/// pool, and the shared door must still be served.
#[test]
fn a_private_and_a_shared_door_coexist() {
    let priv_jamb = DoorPath::new("both_private");
    let shared_jamb = DoorPath::new("both_shared");

    let mut p = Door::builder(Counter)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .private_pool()
        .build(count_proc)
        .expect("build private");
    p.attach(priv_jamb.path()).expect("attach private");

    let mut s = Door::builder(Counter)
        .request_size(0..=64)
        .thread_stack_size(256 * 1024)
        .build(count_proc)
        .expect("build shared");
    s.attach(shared_jamb.path()).expect("attach shared");

    let a = hammer(priv_jamb.path());
    let b = hammer(shared_jamb.path());

    assert_eq!(a, Some(CALLERS), "the private door failed with both open");
    assert_eq!(b, Some(CALLERS), "the shared door failed with both open");
}
