// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A key-value store where every pair belongs to a UNIX group.
//!
//! Build it with:
//!
//! ```sh
//! cargo build --example kvstore --example kvstore_client --features rpc
//! ```
//!
//! # What this example is really about
//!
//! Every pair in the store is owned by one group. To read, change or
//! delete a pair you must be in that group. The server does not ask
//! the client who it is. It asks the kernel, through
//! `door_ucred(3C)`, which [`Request::peer`] wraps.
//!
//! That is the whole point. The kernel fills in the caller's user and
//! group ids from the process that is blocked in `door_call` right
//! now. A client cannot write a different number into that field,
//! because it never touches the field. Over a pipe or a socket you
//! would have to invent a login protocol to get the same thing, and
//! then keep it safe. Here it is free and it cannot be faked.
//!
//! # The shape of this door
//!
//! The task called for `#[door(rpc, ...)]`. That shape hands the
//! method a decoded request and nothing else:
//!
//! ```text
//! fn(&self, Req) -> Result<Resp, E>
//! ```
//!
//! There is no [`Request`] in it, so there is no way to reach
//! [`Request::peer`], and an access-control example cannot be written
//! in it today. So this door uses the `procedure` shape, which does
//! get a [`Request`], and calls `postcard` itself. The wire format is
//! the same one `#[door(rpc)]` produces — serde types encoded with
//! postcard — so the two files below stay about the access control
//! and not about parsing bytes. Two lines do the encoding.
//!
//! # The rules
//!
//! - `Set { key, value, group }` — on create the pair becomes owned
//!   by `group`, and the caller must be in that group. On update the
//!   caller must be in the group that already owns the pair. Owners
//!   never move: `group` is only read when the pair is new.
//! - `Get { key }` — allowed only if the caller is in the owning
//!   group.
//! - `Delete { key }` — same rule as `Get`.
//! - `List` — only the keys the caller is allowed to read. See the
//!   comment on `Op::List` for why.
//!
//! # A worked example
//!
//! Two users, one group, one server. Run steps 1 and 2 as root.
//!
//! ```sh
//! # 1. A test group and two test users. alice is in kvdemo, bob is
//! #    not. New users rather than real ones, so nothing existing is
//! #    disturbed.
//! groupadd kvdemo
//! useradd -m -g staff -G kvdemo alice
//! useradd -m -g staff bob
//! passwd -N alice; passwd -N bob
//!
//! # 2. Read the group's number. The client takes a number, not a
//! #    name, so that this example needs no name lookup code.
//! getent group kvdemo        # kvdemo::1001:alice  ->  gid is 1001
//!
//! # 3. Start the server. Any user can run it.
//! cd /path/to/rusty-doors
//! ./target/debug/examples/kvstore /tmp/kvstore_door &
//!
//! # 4. As alice, who IS in kvdemo. Everything works.
//! su - alice
//! K=/path/to/rusty-doors/target/debug/examples/kvstore_client
//! $K set motd "hello from alice" 1001
//! $K get motd                 # hello from alice
//! $K list                     # motd
//! exit
//!
//! # 5. As bob, who is NOT in kvdemo. Everything is refused, and the
//! #    refusals give nothing away.
//! su - bob
//! K=/path/to/rusty-doors/target/debug/examples/kvstore_client
//! $K get motd                 # no such key      <- not "wrong group"
//! $K delete motd              # no such key
//! $K list                     # prints nothing at all
//! $K set motd "bob was here" 1001   # permission denied
//! ```
//!
//! Note in step 4 that a login is needed after `useradd`. A process
//! is given its group list when it is created, and the kernel reports
//! that list. Editing `/etc/group` does not change a process that is
//! already running.
//!
//! If the server is killed hard, the door stays mounted on its path.
//! Clear it with `fdetach /tmp/kvstore_door` before starting again.

use doors::{Door, NoDescriptors, Request};
use libc::gid_t;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

/// Where the door lives when the command line does not say.
const DEFAULT_PATH: &str = "/tmp/kvstore_door";

// --- the protocol ----------------------------------------------------
//
// `kvstore_client.rs` repeats these two types word for word. They are
// two separate programs, so there is no module to share, and postcard
// writes an enum as its variant number. If you add, remove or reorder
// a variant here, do the same there, or the two will disagree.

/// What a client asks for.
#[derive(Serialize, Deserialize)]
enum Op {
    /// Create or update a pair.
    Set {
        key: String,
        value: String,
        /// The owning group, read only when the pair is new.
        group: gid_t,
    },
    /// Read a pair.
    Get { key: String },
    /// Remove a pair.
    Delete { key: String },
    /// The keys this caller may read.
    List,
}

/// What the server sends back when the call is allowed.
#[derive(Serialize, Deserialize)]
enum Answer {
    /// `Set` and `Delete` worked.
    Done,
    /// The value, from `Get`.
    Value(String),
    /// The keys, from `List`.
    Keys(Vec<String>),
}

// --- errors ----------------------------------------------------------

/// Why a call was refused.
///
/// `doors` turns any error that is `Display` into the reply body, so
/// the client sees these exact words. Choose them with care: the text
/// goes to a caller who may be the one you are keeping out.
#[derive(Debug)]
enum KvError {
    /// The key is not there, **or** the caller is not in its group.
    ///
    /// One error for two different situations, on purpose. See the
    /// comment on `Op::Get`.
    Missing,
    /// A `Set` was refused.
    Denied,
    /// The request bytes did not decode.
    BadRequest,
    /// The reply could not be encoded. Our fault, not the caller's.
    BadReply,
    /// `door_ucred(3C)` failed, so we do not know who called.
    Unknown,
    /// A server thread panicked while holding the lock.
    Poisoned,
}

impl fmt::Display for KvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KvError::Missing => "no such key",
            KvError::Denied => "permission denied",
            KvError::BadRequest => "the request could not be decoded",
            KvError::BadReply => "the reply could not be encoded",
            KvError::Unknown => "the caller could not be identified",
            KvError::Poisoned => "the store is broken",
        })
    }
}

// --- the store -------------------------------------------------------

/// One pair.
struct Entry {
    value: String,
    /// The group that owns this pair.
    ///
    /// Set when the pair is created and never changed after that. An
    /// update cannot hand a pair to a different group, so a caller who
    /// can write a pair still cannot give it away to a group of their
    /// own choosing and lock the real owners out.
    owner: gid_t,
}

/// The door's state.
///
/// Every invocation is handed `&self`, and the kernel may run several
/// server threads at the same time, so the map has to be behind a
/// lock. `&self` is not a hint that calls are serialised; they are
/// not.
struct KvStore {
    entries: Mutex<HashMap<String, Entry>>,
}

impl KvStore {
    fn new() -> Self {
        KvStore {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

#[doors::server]
impl KvStore {
    /// One door for all four operations.
    ///
    /// `refuse_desc` sets `DOOR_REFUSE_DESC`, so the kernel never
    /// delivers a file descriptor to this door. A caller cannot push
    /// an open file at us, and the `NoDescriptors` typestate means
    /// this method has no way to read one even by mistake.
    ///
    /// `request_size = ..=8192` sets `DOOR_PARAM_DATA_MAX`. The kernel
    /// refuses a larger request before any of our code runs, so an
    /// unfriendly client cannot make the server hold a huge buffer.
    #[door(refuse_desc, request_size = ..=8192)]
    fn serve(
        &self,
        mut req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, KvError> {
        // Decode first, credentials second.
        //
        // `peer()` borrows the request mutably, so the credentials and
        // `data()` cannot be held at the same time. `from_bytes`
        // produces an owned `Op`, so the borrow of the request data
        // ends on this line and `peer()` is free to take over.
        let op: Op = postcard::from_bytes(req.data())
            .map_err(|_| KvError::BadRequest)?;

        // Now ask the kernel who is calling.
        //
        // Nothing in `op` says who the caller is, and nothing in it
        // could: the client only fills in `op`. This is the part a
        // client cannot lie about.
        //
        // If the lookup fails we refuse the call. Failing open would
        // mean serving somebody we could not identify, which is worse
        // than failing.
        let caller = req.peer().map_err(|_| KvError::Unknown)?;

        let mut map = self.entries.lock().map_err(|_| KvError::Poisoned)?;

        let answer = match op {
            Op::Set { key, value, group } => {
                // Which group must the caller be in?
                //
                // An existing pair decides for itself: its owner is
                // the group to check, and the `group` field of the
                // request is ignored. Only a brand new pair takes the
                // group from the request.
                let required = match map.get(&key) {
                    Some(entry) => entry.owner,
                    None => group,
                };

                // THE CHECK for Set. One test covers both paths, so
                // the refusal reads the same either way.
                //
                // `is_in_group` looks at the effective gid and the
                // supplementary groups. Comparing only `egid()` would
                // refuse users who really are in the group, and it
                // would do it quietly.
                if !caller.is_in_group(required) {
                    return Err(KvError::Denied);
                }

                map.insert(
                    key,
                    Entry {
                        value,
                        owner: required,
                    },
                );
                Answer::Done
            }

            Op::Get { key } => {
                // THE CHECK for Get, and the reason both arms end in
                // the same error.
                //
                // "you are not in the owning group" and "there is no
                // such key" must look identical from outside. If they
                // did not, anyone who can open the door could ask for
                // a key they cannot read and learn from the error
                // whether it exists. The error would become an oracle
                // over the whole key space. A key name is data too:
                // `payroll.2026.layoffs` tells you plenty with the
                // value still hidden.
                match map.get(&key) {
                    Some(entry) if caller.is_in_group(entry.owner) => {
                        Answer::Value(entry.value.clone())
                    }
                    _ => return Err(KvError::Missing),
                }
            }

            Op::Delete { key } => {
                // THE CHECK for Delete: the same rule as Get, and the
                // same single error, for the same reason.
                let allowed = match map.get(&key) {
                    Some(entry) => caller.is_in_group(entry.owner),
                    None => false,
                };
                if !allowed {
                    return Err(KvError::Missing);
                }

                map.remove(&key);
                Answer::Done
            }

            Op::List => {
                // THE CHECK for List: only the keys this caller could
                // read with Get.
                //
                // This is the safer of the two choices the task
                // offered, and it is the one taken. Listing every key
                // would hand out names to anyone who can open the
                // door, and it would undo all the care taken over the
                // Get error above: you would not need an oracle if
                // the server simply told you.
                let mut keys: Vec<String> = map
                    .iter()
                    .filter(|(_, entry)| caller.is_in_group(entry.owner))
                    .map(|(key, _)| key.clone())
                    .collect();

                // Sorted so the output is the same every run. A
                // HashMap has no order of its own.
                keys.sort();
                Answer::Keys(keys)
            }
        };

        postcard::to_allocvec(&answer).map_err(|_| KvError::BadReply)
    }
}

// --- running it ------------------------------------------------------

/// Create the file the door is attached to, and set its mode.
///
/// The file mode is the outer gate. A user who cannot open this path
/// never reaches the server at all, so the mode is a real part of the
/// policy and not decoration.
///
/// This example uses 0666 so that any user can call the door and see
/// the group check work. A service that only ever serves one group
/// would use 0660 with that group as the owner, and get two gates for
/// the price of one: the file mode keeps most people out cheaply, and
/// the check inside the server is the one that decides.
///
/// The mode is set before `attach`, because `fattach(3C)` mounts the
/// door over this file and the mounted door keeps the mode the file
/// had.
fn make_jamb(path: &str) -> std::io::Result<()> {
    // A leftover file from an earlier run is fine to remove. A
    // leftover *mounted door* is not, and this will fail on one; the
    // header says to run `fdetach` in that case.
    let _ = std::fs::remove_file(path);
    std::fs::File::create(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_PATH.into());

    if let Err(e) = make_jamb(&path) {
        eprintln!("kvstore: cannot prepare {path}: {e}");
        std::process::exit(1);
    }

    let mut door = match Door::builder(KvStore::new())
        .thread_stack_size(256 * 1024)
        .build_serve()
    {
        Ok(door) => door,
        Err(e) => {
            eprintln!("kvstore: cannot create the door: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = door.attach(&path) {
        eprintln!("kvstore: cannot attach to {path}: {e}");
        std::process::exit(1);
    }

    println!("kvstore listening on {path}");
    println!("stop it with Ctrl-C");

    // The door is served by threads the crate creates, so this thread
    // has nothing left to do. It must stay alive anyway: dropping
    // `door` would revoke the door and unmount the path.
    //
    // `park` can wake up on its own, so it goes in a loop.
    loop {
        std::thread::park();
    }
}
