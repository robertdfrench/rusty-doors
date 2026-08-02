// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Minimal reproducer for the intermittent `fattach` failure.
//!
//! See `docs/DESIGN.md` Appendix E. Building and dropping doors from
//! several threads at once makes `Door::attach` fail, usually with
//! `EBADF` and sometimes with `EINVAL`. `EBADF` means the door's own
//! descriptor was closed by somebody else between `door_create`
//! handing it out and `fattach` using it.
//!
//! The test suite hits this because cargo runs its tests in threads.
//! This example is the smallest shape that still hits it, so it can be
//! run under `truss` without a test harness in the way.
//!
//! ```sh
//! cargo run --example fattach_race                # defaults
//! THREADS=8 ROUNDS=400 cargo run --example fattach_race
//! CALL=0 cargo run --example fattach_race         # never call the door
//! ATTACH=0 cargo run --example fattach_race       # never attach it
//! ```
//!
//! Under `truss` the interesting line is a `close()` of the descriptor
//! `door_create` just returned, on a thread that does not own it:
//!
//! ```sh
//! truss -f -t door_create,fattach,close -o /tmp/t.log \
//!     ./target/debug/examples/fattach_race
//! ```
//!
//! Exit status is 0 if every round succeeded, 1 if any `attach`
//! failed.

use doors::__private::{run, NoDescriptors, Outcome};
use doors::server::ReplyProtocol;
use doors::{Client, Door, Request};
use std::ffi::{c_char, c_uint, c_void};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Nothing;

/// SAFETY: registered with `door_create`; the kernel supplies every
/// argument.
unsafe extern "C" fn nothing_proc(
    cookie: *mut c_void,
    argp: *mut c_char,
    arg_size: usize,
    dp: *mut doors::__private::door_desc_t,
    n_desc: c_uint,
) {
    run::<Nothing, NoDescriptors, _, io::Error>(
        cookie,
        argp,
        arg_size,
        dp,
        n_desc,
        4096,
        ReplyProtocol::Tagged,
        |_s: &Nothing, _r: Request<'_, NoDescriptors>| {
            Outcome::bytes(Ok(b"ok".to_vec()))
        },
    )
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let threads = env_usize("THREADS", 8);
    let rounds = env_usize("ROUNDS", 200);
    let do_attach = env_usize("ATTACH", 1) != 0;
    let do_call = env_usize("CALL", 1) != 0;

    println!(
        "threads={threads} rounds={rounds} attach={do_attach} \
         call={do_call}"
    );

    let failures = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for t in 0..threads {
        let failures = Arc::clone(&failures);
        let attempts = Arc::clone(&attempts);

        handles.push(std::thread::spawn(move || {
            for r in 0..rounds {
                // A path of this thread's own, so nothing here is a
                // fight over the same name.
                let path =
                    std::env::temp_dir().join(format!("doors_race_{t}_{r}"));
                let _ = std::fs::remove_file(&path);
                if do_attach && std::fs::write(&path, b"").is_err() {
                    continue;
                }

                let mut door = match Door::builder(Nothing)
                    .request_size(0..=64)
                    .thread_stack_size(256 * 1024)
                    .build(nothing_proc)
                {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[{t}:{r}] build failed: {e}");
                        failures.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };

                attempts.fetch_add(1, Ordering::Relaxed);

                if do_attach {
                    if let Err(e) = door.attach(&path) {
                        eprintln!(
                            "[{t}:{r}] ATTACH FAILED on {}: {e}",
                            path.display()
                        );
                        failures.fetch_add(1, Ordering::Relaxed);
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }

                    if do_call {
                        match Client::open(&path) {
                            Ok(c) => {
                                if let Err(e) = c.call(b"x") {
                                    eprintln!("[{t}:{r}] call: {e}");
                                    failures.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            Err(e) => {
                                eprintln!("[{t}:{r}] open: {e}");
                                failures.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }

                // Dropping here is the other half of the race: this
                // closes a descriptor that another thread's
                // door_create may be about to be handed.
                drop(door);
                let _ = std::fs::remove_file(&path);
            }
        }));
    }

    for h in handles {
        let _ = h.join();
    }

    let f = failures.load(Ordering::Relaxed);
    let a = attempts.load(Ordering::Relaxed);
    println!("attempts={a} failures={f}");

    if f > 0 {
        println!("REPRODUCED");
        std::process::exit(1);
    }
    println!("clean");
}
