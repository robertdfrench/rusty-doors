// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A client for `examples/signer.rs`.
//!
//! ```text
//! signer_client load <name> <key-file>   ask the server to load a key
//!                                        (the server allows root only)
//! signer_client list                     name every loaded key
//! signer_client sign <name> <file|->     sign a document with <name>
//! signer_client log                      read the audit log (root only)
//! ```
//!
//! Two doors are involved, and which one this program opens is the
//! whole point of the example:
//!
//! * `load`, `list` and `log` go to `/var/doorsigner/control.door`.
//! * `sign` goes to `/var/doorsigner/<name>.door`. There is no key name
//!   in that request. The door **is** the key. A user who has been
//!   given one key door can reach that key and nothing else.
//!
//! This program never claims a uid. It cannot: the server asks the
//! kernel who is calling, with `door_ucred(3C)`, and the kernel answers
//! from its own record of this process. Running `load` as a normal user
//! gets a refusal no matter what this program sends.
//!
//! Build it next to the server:
//!
//! ```sh
//! cargo build --example signer --example signer_client
//! ```
//!
//! See the header of `examples/signer.rs` for a worked session, and for
//! the loud warning that anyone may sign in this demo.

use doors::{CallError, Client};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Must match `signer.rs`.
const DOOR_DIR: &str = "/var/doorsigner";
/// Must match `signer.rs`.
const CONTROL_DOOR: &str = "control.door";

// ---------------------------------------------------------------------
// The wire types.
//
// This block is a copy of the one in `signer.rs`, and the two must stay
// identical. A Cargo example is its own crate, so there is nowhere
// shared to put them. In a real program these would live in a small
// crate that both sides depend on.
// ---------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug)]
enum ControlRequest {
    Load { name: String, key_path: String },
    ListKeys,
    ReadLog,
}

#[derive(Serialize, Deserialize, Debug)]
enum ControlReply {
    Loaded {
        door_path: String,
        public_key: String,
    },
    Keys(Vec<KeyInfo>),
    Log(Vec<AuditEntry>),
}

#[derive(Serialize, Deserialize, Debug)]
struct KeyInfo {
    name: String,
    door_path: String,
    public_key: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct SignRequest {
    document: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug)]
struct SignReply {
    key: String,
    sha256: String,
    signature: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AuditEntry {
    unix_time: u64,
    uid: u32,
    gid: u32,
    pid: i32,
    key: String,
    sha256: String,
}

// ---------------------------------------------------------------------

const USAGE: &str = "\
usage:
  signer_client load <name> <key-file>   load a key (server allows root only)
  signer_client list                     list the loaded keys
  signer_client sign <name> <file|->     sign a document ('-' means stdin)
  signer_client log                      print the audit log (root only)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match words.as_slice() {
        ["load", name, key_file] => load(name, key_file),
        ["list"] => list(),
        ["sign", name, document] => sign(name, document),
        ["log"] => log(),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    if let Err(why) = result {
        eprintln!("signer_client: {why}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------

fn load(name: &str, key_file: &str) -> Result<(), String> {
    // The server insists on an absolute path, because it has its own
    // working directory. Make one without touching the file system:
    // `canonicalize` would need permission to look at the key, and this
    // program deliberately never opens it. Whether the key can be read
    // is the server's business, and whether *you* may ask is the
    // server's business too.
    let key_path = match Path::new(key_file) {
        p if p.is_absolute() => p.to_path_buf(),
        p => std::env::current_dir()
            .map_err(|e| format!("no working directory: {e}"))?
            .join(p),
    };

    let reply = call_control(&ControlRequest::Load {
        name: name.to_string(),
        key_path: key_path.display().to_string(),
    })?;

    match reply {
        ControlReply::Loaded {
            door_path,
            public_key,
        } => {
            println!("loaded \"{name}\"");
            println!("  door:       {door_path}");
            println!("  public key: {public_key}");
            println!();
            println!("Anyone who can open that path can sign with this key.");
            Ok(())
        }
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

fn list() -> Result<(), String> {
    match call_control(&ControlRequest::ListKeys)? {
        ControlReply::Keys(keys) if keys.is_empty() => {
            println!("no keys loaded");
            Ok(())
        }
        ControlReply::Keys(keys) => {
            for k in keys {
                println!("{}", k.name);
                println!("  door:       {}", k.door_path);
                println!("  public key: {}", k.public_key);
            }
            Ok(())
        }
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

fn log() -> Result<(), String> {
    match call_control(&ControlRequest::ReadLog)? {
        ControlReply::Log(entries) if entries.is_empty() => {
            println!("the log is empty");
            Ok(())
        }
        ControlReply::Log(entries) => {
            for e in entries {
                // The time is raw seconds since the epoch. The server
                // has no calendar and neither does this crate.
                println!(
                    "{}  uid={} gid={} pid={}  key={}",
                    e.unix_time, e.uid, e.gid, e.pid, e.key
                );
                println!(
                    "  sha256={}  (of the document, not the document)",
                    e.sha256
                );
            }
            Ok(())
        }
        other => Err(format!("unexpected reply: {other:?}")),
    }
}

fn sign(name: &str, document: &str) -> Result<(), String> {
    let bytes = read_document(document)?;

    // This is the structural point of the example. The key name picks a
    // *path*, and the path is what grants access. Nothing in the
    // request below says which key to use.
    let door = PathBuf::from(DOOR_DIR).join(format!("{name}.door"));

    let reply: SignReply = call(&door, &SignRequest { document: bytes })?;

    // The signature on stdout, on its own, so it can be redirected into
    // a file. Everything else goes to stderr.
    eprintln!("key:    {}", reply.key);
    eprintln!("sha256: {}", reply.sha256);
    println!("{}", reply.signature);
    Ok(())
}

/// Read the document to sign: a file, or stdin for `-`.
fn read_document(source: &str) -> Result<Vec<u8>, String> {
    if source == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        Ok(buf)
    } else {
        std::fs::read(source).map_err(|e| format!("{source}: {e}"))
    }
}

// ---------------------------------------------------------------------
// Talking to a door
// ---------------------------------------------------------------------

fn call_control(request: &ControlRequest) -> Result<ControlReply, String> {
    let path = PathBuf::from(DOOR_DIR).join(CONTROL_DOOR);
    call(&path, request)
}

/// One door call: encode, call, decode.
///
/// The encoding is `postcard`, which is what the crate's own
/// `#[door(rpc)]` shape uses, so the wire format is the crate's.
fn call<Req, Resp>(door: &Path, request: &Req) -> Result<Resp, String>
where
    Req: Serialize,
    Resp: serde::de::DeserializeOwned,
{
    let body = postcard::to_allocvec(request)
        .map_err(|e| format!("encoding the request: {e}"))?;

    let client = Client::open(door).map_err(|e| {
        format!(
            "{}: {e}\n(is the signer running, and is that key loaded?)",
            door.display()
        )
    })?;

    // A door says how big a request it takes, through
    // `door_getparam(3C)`. Asking beats guessing: an oversized request
    // comes back from the kernel as a bare `ENOBUFS`, which tells the
    // person running this nothing useful.
    if let Ok(limits) = client.limits() {
        if body.len() > limits.data_max {
            return Err(format!(
                "the request is {} bytes and {} takes at most {}",
                body.len(),
                door.display(),
                limits.data_max
            ));
        }
    }

    let reply = client.call(&body).map_err(describe)?;

    postcard::from_bytes(reply.data())
        .map_err(|e| format!("decoding the reply: {e}"))
}

/// Turn a [`CallError`] into something worth printing.
///
/// The interesting one is `Server`: that is the §3.9 tag 1 reply, which
/// carries the `Display` text of the server's own error type. A refused
/// `load` arrives here.
fn describe(e: CallError) -> String {
    match e {
        CallError::Server { data } => {
            format!("server refused: {}", String::from_utf8_lossy(&data))
        }
        CallError::ServerFailed(fault) => {
            format!("the server broke while handling the call: {fault:?}")
        }
        other => format!("door call failed: {other:?}"),
    }
}
