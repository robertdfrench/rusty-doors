// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Drives the `kvstore` example door.
//!
//! Build it with:
//!
//! ```sh
//! cargo build --example kvstore --example kvstore_client --features rpc
//! ```
//!
//! Usage:
//!
//! ```text
//! kvstore_client set <key> <value> <gid>
//! kvstore_client get <key>
//! kvstore_client delete <key>
//! kvstore_client list
//! ```
//!
//! The door path is `/tmp/kvstore_door`, or `$KV_DOOR` if that is
//! set.
//!
//! # Why there is no `--user` flag
//!
//! There is nothing to pass. The client never says who it is, and it
//! has no way to. The server asks the kernel instead, with
//! `door_ucred(3C)`, and the kernel answers with the real identity of
//! this process. To call as somebody else you must *be* somebody
//! else: `su - bob`, then run this again.
//!
//! That is what makes the example worth reading. The same program,
//! the same bytes on the wire, a different answer — decided entirely
//! by which user ran it.
//!
//! `kvstore.rs` has a full worked example with two users.
//!
//! # Why a group number and not a group name
//!
//! `set` takes the numeric gid. Turning `kvdemo` into `1001` needs
//! `getgrnam(3C)`, which is unsafe FFI and has nothing to do with
//! doors, so it is left out. Read the number with:
//!
//! ```sh
//! getent group kvdemo | cut -d: -f3
//! ```

use doors::{CallError, Client};
use libc::gid_t;
use serde::{Deserialize, Serialize};

/// Where the door lives when `$KV_DOOR` is not set.
const DEFAULT_PATH: &str = "/tmp/kvstore_door";

// --- the protocol ----------------------------------------------------
//
// Copied word for word from `kvstore.rs`. These are two separate
// programs, so there is no module to share. Postcard writes an enum as
// its variant number, so if you add, remove or reorder a variant in
// one file you must do the same in the other.

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

// --- the command line ------------------------------------------------

const USAGE: &str = "\
usage:
  kvstore_client set <key> <value> <gid>
  kvstore_client get <key>
  kvstore_client delete <key>
  kvstore_client list

the door path comes from $KV_DOOR, default /tmp/kvstore_door";

/// Turn the command line into one request, or explain what is wrong.
fn parse(args: &[String]) -> Result<Op, String> {
    let command = args.first().ok_or_else(|| String::from(USAGE))?;

    match (command.as_str(), args.len()) {
        ("set", 4) => {
            let group: gid_t = args[3]
                .parse()
                .map_err(|_| format!("`{}` is not a group number", args[3]))?;
            Ok(Op::Set {
                key: args[1].clone(),
                value: args[2].clone(),
                group,
            })
        }
        ("get", 2) => Ok(Op::Get {
            key: args[1].clone(),
        }),
        ("delete", 2) => Ok(Op::Delete {
            key: args[1].clone(),
        }),
        ("list", 1) => Ok(Op::List),
        _ => Err(String::from(USAGE)),
    }
}

// --- running it ------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let op = match parse(&args) {
        Ok(op) => op,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    let path = std::env::var("KV_DOOR").unwrap_or_else(|_| DEFAULT_PATH.into());

    // Opening the door is the first gate. It obeys the file mode on
    // the door's path, so a user who is not allowed to open it stops
    // here and the server never runs at all.
    let client = match Client::open(&path) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("cannot open {path}: {e}");
            eprintln!("is the kvstore server running?");
            std::process::exit(1);
        }
    };

    let body = match postcard::to_allocvec(&op) {
        Ok(body) => body,
        Err(e) => {
            eprintln!("cannot encode the request: {e}");
            std::process::exit(1);
        }
    };

    // Nothing here carries an identity. The kernel adds that on the
    // way through, and the server reads it there.
    let reply = match client.call(&body) {
        Ok(reply) => reply,

        // The server returned `Err`. Its `Display` text is the whole
        // payload, so print it as it is. This is where "no such key"
        // and "permission denied" arrive.
        Err(CallError::Server { data }) => {
            eprintln!("{}", String::from_utf8_lossy(&data));
            std::process::exit(1);
        }

        Err(e) => {
            eprintln!("call failed: {e}");
            std::process::exit(1);
        }
    };

    let answer: Answer = match postcard::from_bytes(reply.data()) {
        Ok(answer) => answer,
        Err(e) => {
            eprintln!("cannot decode the reply: {e}");
            std::process::exit(1);
        }
    };

    match answer {
        Answer::Done => println!("ok"),
        Answer::Value(value) => println!("{value}"),
        // One key per line, and nothing at all when the caller may
        // read nothing. An empty list is a real answer, not an error.
        Answer::Keys(keys) => {
            for key in keys {
                println!("{key}");
            }
        }
    }
}
