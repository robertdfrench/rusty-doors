// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A document-signing service made of doors.
//!
//! It holds SSH private keys and signs documents with them. It shows
//! two things at once.
//!
//! 1. **An operation only `root` may do.** Loading a key is checked
//!    with `door_ucred(3C)`, through [`Request::peer`]. The kernel
//!    fills those credentials in itself, on the server thread, for the
//!    call that is running right now. A client cannot put a uid in the
//!    request and claim to be root.
//!
//! 2. **One door per key**, instead of one door with a key name in the
//!    request. The door path *is* the capability. You can give somebody
//!    `/var/doorsigner/release.door` and they can sign with that one
//!    key. They cannot reach the other keys, and they never see the key
//!    itself.
//!
//! # THIS IS A DEMO. DO NOT COPY IT INTO A REAL SERVICE.
//!
//! **Any client may sign with any loaded key.** There is no check at
//! all on the signing doors. That is on purpose: it keeps the example
//! about door_ucred and about door layout, and it makes the point that
//! a door with no check is a door with no check.
//!
//! A real service would do one of these, and probably both:
//!
//! * `chown` and `chmod` each key door, so the file system says who may
//!   open it. `fattach(3C)` puts the door behind a normal path, so
//!   normal path permissions apply. That is the whole point of one door
//!   per key.
//! * Check the caller in `sign` the same way `load` does, with
//!   `req.peer()`. `UCred::is_in_group` is the method for "is the
//!   caller in the release-engineering group".
//!
//! # The doors
//!
//! ```text
//! /var/doorsigner/control.door   Load (root only)
//!                                ListKeys (anyone)
//!                                ReadLog (root only)
//!
//! /var/doorsigner/<name>.door    Sign with key <name>
//!                                (anyone -- see the warning above)
//! ```
//!
//! Loading a key creates the second kind of door at run time.
//!
//! # Where the doors live, and why it matters
//!
//! Dropping a [`Door`] revokes it. So every key door has to be held
//! somewhere that lives as long as the process serves it. There is no
//! obvious place: the door is created *inside* a call on the control
//! door, and the local variable dies when that call returns.
//!
//! The answer here is that the key doors live in the control door's own
//! state, in `Signer::keys`. The state is the door's cookie. The crate
//! keeps it alive for as long as the control door exists, and every
//! invocation gets a `&Signer`, so a later call can still find the key
//! doors. The control door outlives them all, which is the right shape:
//! the control door is what created them.
//!
//! It also makes shutdown one step. Dropping the control door drops
//! `Signer`, which drops every key `Door`, and each of those revokes
//! itself and removes its own path. So a `Quit` operation would only
//! have to let `main` return.
//!
//! This server has no `Quit`, so in practice you kill it, and a signal
//! runs no `Drop` at all. The `.door` paths stay attached. Run
//! `fdetach /var/doorsigner/*.door` before starting the server again,
//! or `fattach` will fail with `EBUSY`. That is not a wart in the
//! crate: it is what happens to any `fattach`ed path when the process
//! behind it dies without tidying up.
//!
//! # Signing
//!
//! Real signatures, from `ssh-keygen(1)`:
//!
//! ```text
//! ssh-keygen -Y sign -f <key> -n doorsigner
//! ```
//!
//! The document goes in on standard input, the signature comes out on
//! standard output. This crate pulls in no cryptography of its own, and
//! this is the honest way to use a real SSH key.
//!
//! Standard input matters for a second reason: a document handed over
//! on the command line would show up in `ps(1)` for every user on the
//! machine. Documents may be secret. Arguments are not.
//!
//! # The audit log
//!
//! Every signature is recorded: the caller's uid, gid and pid, the key
//! name, the time, and the **SHA-256 of the document**. Not the
//! document. The document may be secret, and this log exists to answer
//! "who signed something with the release key last Tuesday", not to
//! keep a copy of it. A hash answers that question, because whoever
//! holds the document can hash it and compare.
//!
//! The log is in memory, so it dies with the server. A real one would
//! append to a file opened `O_APPEND`.
//!
//! # Wire format
//!
//! serde types, with `postcard` on the wire. That is the same encoding
//! `#[door(rpc)]` uses, so the request and reply structs below are the
//! interesting part and there is no byte parsing to read past.
//!
//! The doors are `#[door(procedure, ...)]` and not `#[door(rpc, ...)]`,
//! and the reason is worth knowing: an `rpc` method is
//! `fn(&self, Req) -> Result<Resp, E>`. It never sees the `Request`, so
//! it cannot call `Request::peer`, so it cannot ask who the caller is.
//! This whole example is about asking who the caller is. So it takes
//! the `Request` and decodes the body itself, which is two lines.
//!
//! # Building and running
//!
//! ```sh
//! cargo build --example signer --example signer_client
//! ```
//!
//! `/var/doorsigner` must already exist, must be owned by `root`, and
//! must not be writable by group or other. The server checks all three
//! and refuses to start otherwise: anyone who can write that directory
//! can put their own file where a door is about to go.
//!
//! # Worked example
//!
//! ```sh
//! # As root, once.
//! mkdir -p /var/doorsigner
//! chown root:root /var/doorsigner
//! chmod 755 /var/doorsigner
//!
//! # A key to sign with. Keep it to root, mode 600.
//! ssh-keygen -t ed25519 -N '' -C release -f /root/release_key
//!
//! # Start the server, as root, and leave it running.
//! ./target/debug/examples/signer
//!
//! # In another shell, as root: load the key.
//! ./target/debug/examples/signer_client load release /root/release_key
//! # loaded "release"
//! #   door:       /var/doorsigner/release.door
//! #   public key: ssh-ed25519 AAAAC3Nz... release
//!
//! # As anybody at all: list the keys, and sign something.
//! echo 'ship it' > /tmp/note.txt
//! ./target/debug/examples/signer_client list
//! ./target/debug/examples/signer_client sign release /tmp/note.txt
//! # -----BEGIN SSH SIGNATURE-----
//! # ...
//!
//! # As a normal user, loading is refused by the kernel's own idea of
//! # who you are.
//! ./target/debug/examples/signer_client load other /root/other_key
//! # server refused: only root may load a key (you are uid 100)
//!
//! # As root: read the log back.
//! ./target/debug/examples/signer_client log
//! # 1754006400  uid=100 gid=1 pid=1234  key=release
//! #   sha256=1e5c...  (of the document, not the document)
//! ```
//!
//! Verifying a signature afterwards is plain OpenSSH, with no door
//! involved:
//!
//! ```sh
//! ./target/debug/examples/signer_client sign release /tmp/note.txt > /tmp/note.sig
//! echo "signer@example.com $(cat /root/release_key.pub)" > /tmp/allowed
//! ssh-keygen -Y verify -f /tmp/allowed -I signer@example.com \
//!     -n doorsigner -s /tmp/note.sig < /tmp/note.txt
//! ```

use doors::{Door, NoDescriptors, Request};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Where every door in this service lives.
const DOOR_DIR: &str = "/var/doorsigner";

/// The control door, relative to `DOOR_DIR`.
const CONTROL_DOOR: &str = "control.door";

/// The `ssh-keygen -Y` namespace.
///
/// A namespace stops a signature made here from being replayed as a
/// signature of some other kind, such as a `git` commit signature.
/// Verifying with a different `-n` fails.
const SIGN_NAMESPACE: &str = "doorsigner";

/// The largest request any door here accepts, in bytes.
///
/// This is a real limit, not a formality: the kernel copies the request
/// onto the server thread's stack before the procedure runs, so the
/// stack below has to be big enough for it.
const MAX_REQUEST: usize = 64 * 1024;

/// Stack for each door server thread.
///
/// `MAX_REQUEST` for the request, plus room for the procedure itself.
/// `build_*()` refuses a stack that is too small for the declared
/// request size, so this number is checked, not hoped for.
const THREAD_STACK: usize = 256 * 1024;

/// How many log entries `ReadLog` returns.
///
/// A door reply is capped at 64 KiB. The whole log could be larger than
/// that, and a reply that does not fit is an error rather than a short
/// answer, so ask for a bounded number of the newest entries.
const LOG_REPLY_MAX: usize = 200;

// ---------------------------------------------------------------------
// The wire types. `signer_client.rs` has a copy of this block, and the
// two must stay identical: a Cargo example is its own crate, so there
// is nowhere shared to put them.
// ---------------------------------------------------------------------

/// A request to the control door.
#[derive(Serialize, Deserialize, Debug)]
enum ControlRequest {
    /// Load a private key and give it its own door. Root only.
    Load { name: String, key_path: String },
    /// Name every loaded key. Anyone.
    ListKeys,
    /// Read the audit log. Root only.
    ReadLog,
}

/// An answer from the control door.
#[derive(Serialize, Deserialize, Debug)]
enum ControlReply {
    Loaded {
        door_path: String,
        public_key: String,
    },
    Keys(Vec<KeyInfo>),
    Log(Vec<AuditEntry>),
}

/// One loaded key, as `ListKeys` describes it.
///
/// The public key is public, and the door path is not a secret either:
/// knowing a path does not let you open a file you have no permission
/// for. The private key never appears here, and never leaves the
/// server.
#[derive(Serialize, Deserialize, Debug)]
struct KeyInfo {
    name: String,
    door_path: String,
    public_key: String,
}

/// A request to a key door.
#[derive(Serialize, Deserialize, Debug)]
struct SignRequest {
    /// The bytes to sign. There is no key name here: the door the
    /// client opened is the key.
    document: Vec<u8>,
}

/// An answer from a key door.
#[derive(Serialize, Deserialize, Debug)]
struct SignReply {
    key: String,
    /// SHA-256 of the document, in hex. The same value the log holds,
    /// so a client can check what was written about it.
    sha256: String,
    /// The armoured `-----BEGIN SSH SIGNATURE-----` block.
    signature: String,
}

/// One line of the audit log.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct AuditEntry {
    /// Seconds since the epoch. Printed raw: turning that into a date
    /// needs a calendar, and this crate has no room for one.
    unix_time: u64,
    /// The caller's effective uid, from `door_ucred(3C)`.
    uid: u32,
    /// The caller's effective gid, from `door_ucred(3C)`.
    gid: u32,
    /// The caller's pid, from `door_ucred(3C)`. Only meaningful while
    /// the caller was blocked in `door_call`; pids get reused.
    pid: i32,
    key: String,
    /// SHA-256 of the document, in hex. Never the document.
    sha256: String,
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Everything a door here can refuse to do.
///
/// `Display` is all the crate needs: it implements `ErrorReply` for
/// anything that is `Display`, and the client sees the text as
/// `CallError::Server`.
///
/// Every message is written to be safe to show a stranger. Nothing here
/// repeats a path from the server's own configuration or the output of
/// a program the server ran. Those go to the server's stderr instead.
#[derive(Debug)]
enum SignerError {
    /// The caller is not root and asked for something only root may do.
    NotRoot { operation: &'static str, uid: u32 },
    /// `door_ucred(3C)` would not say who the caller is. Refuse: an
    /// access check that cannot see the caller has to fail closed.
    NoCredentials,
    /// The request did not decode.
    BadRequest,
    /// The reply did not encode. A server bug, not a client one.
    BadReply,
    /// A key name that cannot be part of a path.
    BadKeyName,
    /// A key path that the server will not accept.
    BadKeyPath(&'static str),
    /// A name the server keeps for itself.
    ReservedKeyName,
    /// That name is already loaded.
    AlreadyLoaded,
    /// A helper program failed. Details went to the server's stderr.
    ToolFailed(&'static str),
    /// Something on the server side broke.
    Internal(&'static str),
}

impl std::fmt::Display for SignerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignerError::NotRoot { operation, uid } => {
                write!(f, "only root may {operation} (you are uid {uid})")
            }
            SignerError::NoCredentials => {
                write!(f, "the server could not identify the caller")
            }
            SignerError::BadRequest => write!(f, "malformed request"),
            SignerError::BadReply => write!(f, "the reply could not be built"),
            SignerError::BadKeyName => write!(
                f,
                "a key name must be 1 to 64 characters of a-z, A-Z, 0-9, \
                 '-' or '_'"
            ),
            SignerError::BadKeyPath(why) => {
                write!(f, "the key path is not acceptable: {why}")
            }
            SignerError::ReservedKeyName => {
                write!(f, "that key name is reserved by the server")
            }
            SignerError::AlreadyLoaded => {
                write!(f, "a key with that name is already loaded")
            }
            SignerError::ToolFailed(tool) => {
                write!(f, "{tool} failed; see the server's log")
            }
            SignerError::Internal(what) => {
                write!(f, "the server failed: {what}")
            }
        }
    }
}

// ---------------------------------------------------------------------
// Who called
// ---------------------------------------------------------------------

/// The caller's identity, copied out of a [`doors::UCred`].
///
/// Copying is not an optimisation, it is what the API asks for.
/// `Request::peer()` borrows the `Request` mutably, so a live `UCred`
/// and a call to `req.data()` cannot both exist. Every field is a plain
/// integer, so take the credentials first, copy the three numbers out,
/// and let the `UCred` go. Then the payload is reachable again.
#[derive(Clone, Copy, Debug)]
struct Caller {
    uid: u32,
    gid: u32,
    pid: i32,
}

impl Caller {
    /// Ask the kernel who is on the other end of this call.
    ///
    /// This is the security-relevant line in the whole file. The values
    /// come from `door_ucred(3C)`, which reads them from the kernel's
    /// record of the calling thread. They are not in the request, so a
    /// client cannot choose them, and there is nothing to forge.
    fn of(req: &mut Request<'_, NoDescriptors>) -> Result<Caller, SignerError> {
        let peer = req.peer().map_err(|_| SignerError::NoCredentials)?;
        Ok(Caller {
            uid: peer.euid(),
            gid: peer.egid(),
            pid: peer.pid(),
        })
        // The `UCred` dies here, which frees the borrow on `req`.
    }

    /// Effective uid 0.
    ///
    /// The *effective* uid is the right one: it is what the kernel uses
    /// for its own permission checks, and it is what a set-uid program
    /// changes. Checking the real uid would let a program that has
    /// dropped privileges keep them here.
    fn is_root(self) -> bool {
        self.uid == 0
    }

    /// Fail unless the caller is root.
    fn require_root(self, operation: &'static str) -> Result<(), SignerError> {
        if self.is_root() {
            Ok(())
        } else {
            Err(SignerError::NotRoot {
                operation,
                uid: self.uid,
            })
        }
    }
}

// ---------------------------------------------------------------------
// The control door
// ---------------------------------------------------------------------

/// The state behind `/var/doorsigner/control.door`.
///
/// This is the door's cookie. Every invocation gets a `&Signer`, and it
/// lives as long as the door does.
struct Signer {
    /// Where new key doors are created.
    dir: PathBuf,
    /// Loaded keys, by name. **This owns the key doors.** See the
    /// module header: it is the only place they can live.
    keys: Mutex<BTreeMap<String, LoadedKey>>,
    /// Shared with every key door, because both sides touch it: the key
    /// doors append, the control door reads.
    log: Arc<Mutex<Vec<AuditEntry>>>,
}

/// One loaded key and the door that serves it.
struct LoadedKey {
    door_path: PathBuf,
    public_key: String,
    /// Held only to keep the door open. Dropping a `Door` revokes it,
    /// removes its path, and any client mid-call gets an error. So this
    /// field is never read, and that is correct.
    _door: Door<SigningKey>,
}

#[doors::server]
impl Signer {
    /// The control door.
    ///
    /// `refuse_desc` sets `DOOR_REFUSE_DESC`, so the kernel will not
    /// deliver a file descriptor to this door at all. A client cannot
    /// slip one in. The `NoDescriptors` type says the same thing in
    /// Rust: there is no method here that could reach a descriptor.
    #[door(procedure, refuse_desc, request_size = 0..=MAX_REQUEST)]
    fn control(
        &self,
        mut req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, SignerError> {
        // Credentials first, payload second. `peer()` borrows `req`
        // mutably, so this order is not a style choice.
        let who = Caller::of(&mut req)?;

        let request: ControlRequest = postcard::from_bytes(req.data())
            .map_err(|_| SignerError::BadRequest)?;

        let reply = match request {
            ControlRequest::Load { name, key_path } => {
                self.load(who, &name, Path::new(&key_path))?
            }
            ControlRequest::ListKeys => self.list_keys(),
            ControlRequest::ReadLog => self.read_log(who)?,
        };

        postcard::to_allocvec(&reply).map_err(|_| SignerError::BadReply)
    }
}

impl Signer {
    /// Load a private key and give it a door of its own. Root only.
    fn load(
        &self,
        who: Caller,
        name: &str,
        key_path: &Path,
    ) -> Result<ControlReply, SignerError> {
        who.require_root("load a key")?;

        // The name becomes a file name under `self.dir`. An unchecked
        // name here is a path traversal: "../../etc/shadow" would make
        // the server create a jamb file wherever the caller liked. Only
        // root gets this far, but a check that only holds because of
        // another check is a check waiting to be moved.
        if !is_safe_key_name(name) {
            return Err(SignerError::BadKeyName);
        }

        // An absolute path, so the answer does not depend on the
        // server's working directory. Also: `ssh-keygen` would read a
        // leading '-' as an option, and refusing relative paths refuses
        // those too.
        if !key_path.is_absolute() {
            return Err(SignerError::BadKeyPath("it must be absolute"));
        }

        // Reading the public key out proves three things at once: the
        // file exists, the server can read it, and it is not protected
        // by a passphrase. Better to find that out now than on the
        // first signature.
        let public_key = read_public_key(key_path)?;

        let mut keys = lock(&self.keys);
        if keys.contains_key(name) {
            return Err(SignerError::AlreadyLoaded);
        }

        let door_path = self.dir.join(format!("{name}.door"));

        // A key called "control" would land on the control door's own
        // path. Compare the paths rather than the names, so this stays
        // right if either name is ever changed.
        if door_path == self.dir.join(CONTROL_DOOR) {
            return Err(SignerError::ReservedKeyName);
        }

        // `fattach(3C)` covers an existing file, the way a mount covers
        // a directory. So there has to be a file there first.
        let created = !door_path.exists();
        if created {
            std::fs::File::create(&door_path).map_err(|_| {
                SignerError::Internal("could not create the door path")
            })?;
        }

        // SECURITY: read this twice. These permissions are the access
        // control on this key. The door is reached by opening the path,
        // so whoever may open the path may sign with the key.
        //
        // 0666 lets everybody sign, which is what this demo says it
        // does and why the module header shouts about it. In a real
        // service this line is where the key's policy goes: 0660 with
        // `chown root:release`, and only that group can sign.
        std::fs::set_permissions(
            &door_path,
            std::fs::Permissions::from_mode(0o666),
        )
        .map_err(|_| SignerError::Internal("could not set the door mode"))?;

        let mut door = Door::builder(SigningKey {
            name: name.to_string(),
            key_path: key_path.to_path_buf(),
            log: Arc::clone(&self.log),
        })
        .thread_stack_size(THREAD_STACK)
        .build_sign()
        .map_err(|e| {
            eprintln!("signer: build_sign for {name}: {e}");
            SignerError::Internal("could not create the key door")
        })?;

        if let Err(e) = door.attach(&door_path) {
            eprintln!("signer: attach {}: {e}", door_path.display());
            // Remove only a file we made ourselves. A path that was
            // already there may belong to somebody else, and a door
            // left attached by a killed server has to be cleared with
            // `fdetach`, not unlinked.
            if created {
                let _ = std::fs::remove_file(&door_path);
            }
            return Err(SignerError::Internal("could not attach the key door"));
        }

        let reply = ControlReply::Loaded {
            door_path: door_path.display().to_string(),
            public_key: public_key.clone(),
        };

        // Moving the `Door` in here is what keeps it alive. If this
        // line were left out the door would be revoked at the end of
        // this function and the path would go with it.
        keys.insert(
            name.to_string(),
            LoadedKey {
                door_path,
                public_key,
                _door: door,
            },
        );

        eprintln!("signer: loaded key {name} for uid {}", who.uid);
        Ok(reply)
    }

    /// Name every loaded key. Anyone may ask.
    fn list_keys(&self) -> ControlReply {
        let keys = lock(&self.keys);
        ControlReply::Keys(
            keys.iter()
                .map(|(name, k)| KeyInfo {
                    name: name.clone(),
                    door_path: k.door_path.display().to_string(),
                    public_key: k.public_key.clone(),
                })
                .collect(),
        )
    }

    /// Read the audit log back. Root only.
    ///
    /// The log says who signed what. That is a record of other people's
    /// activity, so it is not something every caller should be able to
    /// read.
    fn read_log(&self, who: Caller) -> Result<ControlReply, SignerError> {
        who.require_root("read the audit log")?;

        let log = lock(&self.log);
        let start = log.len().saturating_sub(LOG_REPLY_MAX);
        Ok(ControlReply::Log(log[start..].to_vec()))
    }
}

// ---------------------------------------------------------------------
// One door per key
// ---------------------------------------------------------------------

/// The state behind `/var/doorsigner/<name>.door`.
///
/// One of these per loaded key. The key name is here, not in the
/// request, because the door itself picks the key.
struct SigningKey {
    name: String,
    /// The private key file. Read at signing time, by `ssh-keygen`. The
    /// bytes never enter this process.
    key_path: PathBuf,
    /// The same log the control door reads.
    log: Arc<Mutex<Vec<AuditEntry>>>,
}

#[doors::server]
impl SigningKey {
    /// Sign a document with this door's key.
    ///
    /// **No access check.** See the warning in the module header. The
    /// caller is identified anyway, because the log needs it, so adding
    /// the check would be one `who.require_root(...)`-shaped line.
    #[door(procedure, refuse_desc, request_size = 0..=MAX_REQUEST)]
    fn sign(
        &self,
        mut req: Request<'_, NoDescriptors>,
    ) -> Result<Vec<u8>, SignerError> {
        // Credentials first, then the payload: `peer()` holds a mutable
        // borrow of `req` until the `Caller` is built.
        let who = Caller::of(&mut req)?;

        let request: SignRequest = postcard::from_bytes(req.data())
            .map_err(|_| SignerError::BadRequest)?;

        let sha256 = sha256_hex(&request.document)?;
        let signature = ssh_sign(&self.key_path, &request.document)?;

        // Log it before replying. If the reply cannot be built, the
        // signature still happened, and the log should say so.
        lock(&self.log).push(AuditEntry {
            unix_time: now_secs(),
            uid: who.uid,
            gid: who.gid,
            pid: who.pid,
            key: self.name.clone(),
            sha256: sha256.clone(),
        });

        let reply = SignReply {
            key: self.name.clone(),
            sha256,
            signature,
        };
        postcard::to_allocvec(&reply).map_err(|_| SignerError::BadReply)
    }
}

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// Is this name safe to put in a path?
///
/// Allowing only these characters rules out `/`, `..`, the empty name,
/// and anything a shell would find interesting. It is a list of what is
/// allowed, not a list of what is banned, which is the way round that
/// stays correct when somebody thinks of a new attack.
fn is_safe_key_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Take a lock, and keep going if it is poisoned.
///
/// A poisoned lock means an earlier call panicked while holding it. The
/// data behind these locks is a plain map and a plain list, so nothing
/// is half-updated, and refusing every later call would turn one bad
/// request into an outage.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Seconds since the epoch.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// SHA-256 of some bytes, in hex, from `digest(1)`.
fn sha256_hex(bytes: &[u8]) -> Result<String, SignerError> {
    let mut cmd = Command::new("digest");
    cmd.arg("-a").arg("sha256");
    let out = run_capture(cmd, bytes, "digest")?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// The public half of a private key, from `ssh-keygen -y`.
fn read_public_key(key_path: &Path) -> Result<String, SignerError> {
    let mut cmd = ssh_keygen();
    cmd.arg("-y").arg("-f").arg(key_path);
    let out = run_capture(cmd, b"", "ssh-keygen -y")?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Sign `document` with `key_path`, using `ssh-keygen -Y sign`.
fn ssh_sign(key_path: &Path, document: &[u8]) -> Result<String, SignerError> {
    let mut cmd = ssh_keygen();
    cmd.arg("-Y")
        .arg("sign")
        .arg("-f")
        .arg(key_path)
        .arg("-n")
        .arg(SIGN_NAMESPACE);
    // The document goes in on stdin, never as an argument: arguments
    // are visible to every user on the machine through `ps(1)`.
    let out = run_capture(cmd, document, "ssh-keygen -Y sign")?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// An `ssh-keygen` that can never stop and ask for a passphrase.
///
/// A door server has no terminal, and a call that blocks forever on a
/// passphrase prompt would tie up a server thread with nobody able to
/// answer it. `SSH_ASKPASS_REQUIRE=force` makes OpenSSH use the askpass
/// program instead of `/dev/tty`, and `/bin/false` as that program
/// makes it fail at once. So a passphrase-protected key is an error,
/// which is what we want.
fn ssh_keygen() -> Command {
    let mut cmd = Command::new("ssh-keygen");
    cmd.env("SSH_ASKPASS", "/bin/false");
    cmd.env("SSH_ASKPASS_REQUIRE", "force");
    cmd.env("DISPLAY", "");
    cmd
}

/// Run a program, feed it `input`, and collect its standard output.
///
/// Two things here are deliberate.
///
/// The arguments are passed as an argument list, never through a shell.
/// There is no string for a `;` or a `$(...)` to hide in, so a name or
/// a path cannot turn into a command.
///
/// The child's standard error goes to the server's own standard error
/// and **not** back to the client. It can name paths and other server
/// details, and the client may be anybody.
fn run_capture(
    mut cmd: Command,
    input: &[u8],
    label: &'static str,
) -> Result<Vec<u8>, SignerError> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        eprintln!("signer: could not run {label}: {e}");
        SignerError::ToolFailed(label)
    })?;

    // Write everything, then close the pipe so the child sees EOF.
    // Writing first and reading after is safe here only because both
    // `digest` and `ssh-keygen` read all of their input before they
    // write anything. A program that answered as it read could fill the
    // output pipe while we were still writing, and both sides would
    // wait forever. That one would need a thread.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or(SignerError::Internal("no stdin pipe"))?;
        if let Err(e) = stdin.write_all(input) {
            eprintln!("signer: writing to {label}: {e}");
            let _ = child.wait();
            return Err(SignerError::ToolFailed(label));
        }
    }

    let out = child.wait_with_output().map_err(|e| {
        eprintln!("signer: waiting for {label}: {e}");
        SignerError::ToolFailed(label)
    })?;

    if !out.status.success() {
        eprintln!(
            "signer: {label} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return Err(SignerError::ToolFailed(label));
    }
    Ok(out.stdout)
}

// ---------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------

/// Refuse to start unless `dir` is safe to put doors in.
///
/// Anybody who can write this directory can create the file that a door
/// is about to be attached to, or replace one afterwards. That makes
/// the directory's permissions part of this service's security, so they
/// are checked rather than assumed.
fn check_door_dir(dir: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;

    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if meta.uid() != 0 {
        return Err(format!(
            "{} is owned by uid {}, and must be owned by root",
            dir.display(),
            meta.uid()
        ));
    }
    // 0o022 is "group write" plus "other write".
    if meta.permissions().mode() & 0o022 != 0 {
        return Err(format!(
            "{} is writable by group or other (mode {:o}); use 755",
            dir.display(),
            meta.permissions().mode() & 0o7777
        ));
    }
    Ok(())
}

fn main() {
    let dir = PathBuf::from(DOOR_DIR);

    if let Err(why) = check_door_dir(&dir) {
        eprintln!("signer: {why}");
        eprintln!("signer: see the header of examples/signer.rs");
        std::process::exit(1);
    }

    // SAFETY: `geteuid` takes no arguments, reads no memory through a
    // pointer, and is documented never to fail.
    let me = unsafe { libc::geteuid() };
    if me != 0 {
        eprintln!("signer: run me as root (I am uid {me})");
        eprintln!("signer: only root can read the private keys");
        std::process::exit(1);
    }

    let control_path = dir.join(CONTROL_DOOR);

    // The jamb: `fattach` needs a file to cover.
    if !control_path.exists() {
        if let Err(e) = std::fs::File::create(&control_path) {
            eprintln!("signer: {}: {e}", control_path.display());
            std::process::exit(1);
        }
    }
    // Anyone may talk to the control door. The operations behind it
    // check the caller themselves, with `door_ucred(3C)`, which is a
    // better check than "can you open this file": it survives the file
    // being copied, moved, or handed to somebody else.
    if let Err(e) = std::fs::set_permissions(
        &control_path,
        std::fs::Permissions::from_mode(0o666),
    ) {
        eprintln!("signer: chmod {}: {e}", control_path.display());
        std::process::exit(1);
    }

    let mut door = match Door::builder(Signer {
        dir: dir.clone(),
        keys: Mutex::new(BTreeMap::new()),
        log: Arc::new(Mutex::new(Vec::new())),
    })
    .thread_stack_size(THREAD_STACK)
    .build_control()
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("signer: could not create the control door: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = door.attach(&control_path) {
        eprintln!("signer: attach {}: {e}", control_path.display());
        eprintln!(
            "signer: if a previous run was killed, try: fdetach {}",
            control_path.display()
        );
        std::process::exit(1);
    }

    println!("signer: serving {}", control_path.display());
    println!("signer: load a key with: signer_client load <name> <keyfile>");

    // Doors are served by kernel-created threads, so the main thread
    // has nothing to do except stay alive. It also has to keep `door`
    // alive: dropping it would revoke the control door and, through
    // `Signer`, every key door with it.
    loop {
        std::thread::park();
    }
}
