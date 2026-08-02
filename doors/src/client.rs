// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Calling doors.

use crate::descriptor::{
    DescriptorPolicy, Descriptors, NoDescriptors, ReceivedFd, SentFd,
};
use crate::error::{CallError, Error, ServerFault, StatusTag};
use crate::server::DoorInfo;
use crate::sys;
use crate::types::{door_arg_t, door_desc_t};
use doors_sys::{
    DOOR_PARAM_DATA_MAX, DOOR_PARAM_DATA_MIN, DOOR_PARAM_DESC_MAX,
    DOOR_REFUSE_DESC,
};
use std::ffi::{c_char, c_void, CString};
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

/// A mapping the kernel made for a reply, unmapped on `Drop`.
///
/// Its own type, with its own `Drop`, so that unmapping does not
/// depend on any other field of [`Reply`] being in a particular state
/// — `GOALS.md` §12.5 says `Reply` always unmaps, and the simplest way
/// to always is to make it the only thing this type does.
#[derive(Debug)]
struct Mapping {
    base: *mut c_void,
    len: usize,
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: base/len came back from door_call as the region it
        // mapped for us, and nothing else can unmap it: Mapping is not
        // Clone and Reply owns exactly one.
        unsafe {
            libc::munmap(self.base, self.len);
        }
    }
}

/// A reply from a door.
///
/// Client-side only. A server procedure returns `Result<Vec<u8>, E>`
/// or writes into a [`ReplyBuf`](crate::server::ReplyBuf); it never
/// builds one of these.
///
/// If the kernel mapped fresh pages to hold the reply, this owns them
/// and unmaps them on `Drop`, unconditionally. [`data`](Reply::data)
/// borrows from the mapping, so the borrow checker will not let a
/// slice outlive it.
#[derive(Debug)]
pub struct Reply {
    /// Dropped last, after `descriptors` — but descriptors are
    /// extracted at construction, so nothing borrows from here.
    _map: Option<Mapping>,
    data: *const u8,
    len: usize,
    descriptors: Vec<ReceivedFd>,
}

// SAFETY: the mapping is process-wide and `Reply` owns it exclusively;
// there is no thread affinity in an anonymous mapping or an OwnedFd.
unsafe impl Send for Reply {}
// SAFETY: `&Reply` only ever reads the mapping.
unsafe impl Sync for Reply {}

impl Reply {
    /// The reply payload.
    ///
    /// Borrowed from this `Reply`, so it cannot outlive the mapping it
    /// points into.
    pub fn data(&self) -> &[u8] {
        if self.data.is_null() || self.len == 0 {
            return &[];
        }
        // SAFETY: data/len describe the reply the kernel delivered,
        // inside a mapping this Reply keeps alive for '_.
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }

    /// Descriptors the server sent, if any.
    ///
    /// Extracted from the mapped region before it was unmapped, so
    /// these stay valid for the life of the `Reply`.
    pub fn descriptors(&self) -> &[ReceivedFd] {
        &self.descriptors
    }

    /// Take the descriptors, leaving the payload behind.
    pub fn into_descriptors(self) -> Vec<ReceivedFd> {
        self.descriptors
    }

    /// Copy the payload out, so it can outlive the mapping.
    pub fn to_vec(&self) -> Vec<u8> {
        self.data().to_vec()
    }
}

/// The limits a door declares, via `door_getparam(3C)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorParams {
    /// Largest request the door accepts, in bytes.
    pub data_max: usize,
    /// Smallest request the door accepts, in bytes.
    pub data_min: usize,
    /// Most descriptors the door accepts per call.
    pub desc_max: usize,
}

/// A handle for calling a door.
///
/// The `D` parameter decides whether this client may send or receive
/// descriptors. It starts at [`NoDescriptors`], because accepting them
/// means taking on cleanup that a caller who never wanted them would
/// have to remember to do.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use doors::Client;
///
/// let client = Client::open("/var/run/my_door")?;
/// let reply = client.call(b"hello")?;
/// println!("{:?}", reply.data());
/// # Ok(())
/// # }
/// ```
///
/// # Talking to a door this crate did not create
///
/// By default the reply carries one extra byte in front of it. Two
/// peers that both use this crate use that byte to tell an error from a
/// success. A door written in C does not write it, and does not expect
/// it. For those doors, call [`untagged`](Client::untagged) first: the
/// reply then comes back exactly as it arrived, with nothing read off
/// the front. Keep the default when the other side also uses this
/// crate, because the extra byte is what makes the richer errors
/// possible.
///
/// The descriptor is private: `Client` implements neither `AsRawFd`
/// nor `IntoRawFd` (`GOALS.md` §12.6), because handing it out would
/// let someone `close` it or `door_call` it behind the typestate's
/// back.
#[derive(Debug)]
pub struct Client<D = NoDescriptors> {
    fd: OwnedFd,
    _d: PhantomData<D>,
}

impl Client<NoDescriptors> {
    /// Open a door by path.
    ///
    /// The descriptor gets `FD_CLOEXEC`, so an `exec` in this process
    /// does not silently hand the door to the new program. Use
    /// [`open_inheritable`](Client::open_inheritable) when that is
    /// what you want.
    pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        Self::open_inner(path.as_ref(), true)
    }

    /// Open a door whose descriptor survives `exec`.
    ///
    /// The explicit opt-out `GOALS.md` §6.1 asks for. Prefer
    /// [`open`](Client::open) unless a child program genuinely needs
    /// to inherit this door.
    pub fn open_inheritable<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        Self::open_inner(path.as_ref(), false)
    }

    fn open_inner(path: &Path, cloexec: bool) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;

        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from(Error::PathHasNul))?;

        let mut flags = libc::O_RDONLY;
        if cloexec {
            flags |= libc::O_CLOEXEC;
        }

        // SAFETY: c is NUL-terminated and lives across the call.
        let raw = unsafe { libc::open(c.as_ptr(), flags) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }

        // SAFETY: open() just gave us this descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        Ok(Client {
            fd,
            _d: PhantomData,
        })
    }

    /// Opt in to sending and receiving descriptors.
    ///
    /// Fails with [`Error::RefusesDescriptors`] when the door was
    /// created with `DOOR_REFUSE_DESC`. That check is why this is
    /// fallible: the door has already told us it will not accept them,
    /// so there is no reason to let a caller build a client that can
    /// only ever fail.
    pub fn with_descriptors(self) -> Result<Client<Descriptors>, Error> {
        let info = self.info()?;
        if info.attributes() & DOOR_REFUSE_DESC != 0 {
            return Err(Error::RefusesDescriptors);
        }
        Ok(Client {
            fd: self.fd,
            _d: PhantomData,
        })
    }
}

impl<D: DescriptorPolicy> Client<D> {
    /// What the kernel knows about this door.
    pub fn info(&self) -> Result<DoorInfo, Error> {
        crate::server::door_info_for(self.fd.as_raw_fd())
    }

    /// The door's declared limits, via `door_getparam(3C)`.
    pub fn limits(&self) -> Result<DoorParams, Error> {
        let fd = self.fd.as_raw_fd();
        Ok(DoorParams {
            data_max: getparam(fd, DOOR_PARAM_DATA_MAX)?,
            data_min: getparam(fd, DOOR_PARAM_DATA_MIN)?,
            desc_max: getparam(fd, DOOR_PARAM_DESC_MAX)?,
        })
    }

    /// Read replies as plain bytes, with no status tag.
    ///
    /// Returns a small view of this client. It has the same call
    /// methods, but it never reads a status byte off the front of a
    /// reply. Use it to call a door this crate did not create, such as
    /// a door written in C, which knows nothing about that byte.
    ///
    /// The view only borrows the client. It holds no state and costs
    /// nothing at run time; it just picks a different way to read the
    /// reply.
    pub fn untagged(&self) -> Untagged<'_, D> {
        Untagged { client: self }
    }

    /// Call the door.
    ///
    /// Does **not** retry on `EINTR`; an interrupted call comes back as
    /// [`CallError::Interrupted`] because the server procedure may
    /// already have run and retrying would run it twice. Use
    /// [`call_idempotent`](Client::call_idempotent) when running twice
    /// is genuinely harmless.
    pub fn call(&self, data: &[u8]) -> Result<Reply, CallError> {
        let reply = self.call_raw(data, Vec::new(), None)?;
        decode_tagged(reply)
    }

    /// Call the door, retrying while it is interrupted.
    ///
    /// **An interrupted call may already have run.** `door_call`
    /// cannot tell you whether the server procedure completed before
    /// the interruption, so every retry risks executing it again. Only
    /// use this where a second execution is harmless.
    pub fn call_idempotent(&self, data: &[u8]) -> Result<Reply, CallError> {
        loop {
            match self.call_raw(data, Vec::new(), None) {
                Err(CallError::Interrupted) => continue,
                other => return other.and_then(decode_tagged),
            }
        }
    }

    /// Call the door, writing the reply into `buf`.
    ///
    /// Avoids a mapping entirely when the reply fits. When it does not,
    /// the kernel's overflow mapping is unmapped **before this
    /// returns**, and the caller gets
    /// [`CallError::ReplyTooBig`] with the size it would have needed.
    /// There is no path on which the caller observes a mapping here.
    pub fn call_into<'b>(
        &self,
        data: &[u8],
        buf: &'b mut [u8],
    ) -> Result<&'b [u8], CallError> {
        let base = buf.as_mut_ptr();
        let cap = buf.len();
        let reply = self.call_raw(data, Vec::new(), Some((base, cap)))?;

        // call_raw with a caller buffer never yields a mapping: it
        // turns overflow into ReplyTooBig before returning.
        debug_assert!(reply._map.is_none());

        // The reply landed somewhere inside `buf`, and its first byte
        // is the §3.9 status tag. Work out where, so the slice we hand
        // back is the payload and not the tag.
        let start = (reply.data as usize)
            .checked_sub(base as usize)
            .ok_or(CallError::Protocol("reply landed outside the buffer"))?;
        let len = reply.len;
        if start + len > cap {
            return Err(CallError::Protocol("reply overruns the buffer"));
        }

        let (tag, payload) = buf[start..start + len]
            .split_first()
            .ok_or(CallError::Protocol("reply was empty; expected a tag"))?;

        match StatusTag::from_byte(*tag) {
            Some(StatusTag::Ok) => {
                let n = payload.len();
                Ok(&buf[start + 1..start + 1 + n])
            }
            Some(StatusTag::UserError) => Err(CallError::Server {
                data: payload.to_vec(),
            }),
            Some(StatusTag::Fault) => Err(CallError::ServerFailed(
                payload
                    .first()
                    .and_then(|b| ServerFault::from_byte(*b))
                    .unwrap_or(ServerFault::Panicked),
            )),
            None => Err(CallError::Protocol("unrecognised status tag")),
        }
    }

    /// The shared body behind every call method.
    ///
    /// `rbuf` is the caller's result area, if it has one. Passing
    /// `None` lets the kernel map whatever it needs, which the
    /// returned [`Reply`] then owns.
    fn call_raw(
        &self,
        data: &[u8],
        fds: Vec<SentFd<'_>>,
        rbuf: Option<(*mut u8, usize)>,
    ) -> Result<Reply, CallError> {
        // --- Step 1: give up ownership of every Released descriptor.
        //
        // After this loop no OwnedFd for them exists, so no matter
        // what the kernel does there is nothing left to double close.
        let mut descs: Vec<door_desc_t> = Vec::with_capacity(fds.len());
        let mut released: Vec<RawFd> = Vec::new();
        for f in fds {
            let (raw, attrs, was_released) = f.into_raw();
            if was_released {
                released.push(raw);
            }
            // SAFETY: zeroed is a valid door_desc_t; we then set the
            // fields the kernel reads. Writes go through raw pointers
            // because the struct is packed.
            let mut d: door_desc_t = unsafe { std::mem::zeroed() };
            d.d_attributes = attrs;
            d.d_data.d_desc.d_descriptor = raw;
            descs.push(d);
        }

        let (rbuf_ptr, rsize) = match rbuf {
            Some((p, len)) => (p as *mut c_char, len),
            None => (std::ptr::null_mut(), 0usize),
        };

        let mut arg = door_arg_t {
            // door_call does not write through data_ptr; it replaces
            // the field with wherever the results landed.
            data_ptr: data.as_ptr() as *mut c_char,
            data_size: data.len(),
            desc_ptr: if descs.is_empty() {
                std::ptr::null_mut()
            } else {
                descs.as_mut_ptr()
            },
            desc_num: descs.len() as u32,
            rbuf: rbuf_ptr,
            rsize,
        };
        let supplied_rbuf = arg.rbuf;

        // SAFETY: every pointer above is either null with a zero count
        // or points at live memory that outlives the call.
        let rc = unsafe { sys::door_call(self.fd.as_raw_fd(), &mut arg) };

        if rc < 0 {
            let errno = sys::last_errno();
            return Err(classify_call_failure(errno, released));
        }

        // --- Steps 2 and 3: the call went through, so the kernel owns
        // every Released descriptor now. Forget them without closing.
        drop(released);

        self.build_reply(arg, supplied_rbuf)
    }

    /// Turn a completed `door_arg_t` into a [`Reply`], honouring the
    /// descriptor typestate.
    fn build_reply(
        &self,
        arg: door_arg_t,
        supplied_rbuf: *mut c_char,
    ) -> Result<Reply, CallError> {
        // Did the kernel map fresh pages, or use the buffer we gave it?
        let mapped = !arg.rbuf.is_null() && arg.rbuf != supplied_rbuf;

        // Extract descriptors FIRST. They live inside the mapped
        // region, so reading them after unmapping would be a
        // use-after-free (GOALS.md §6.2).
        let mut received: Vec<ReceivedFd> = Vec::new();
        if !arg.desc_ptr.is_null() && arg.desc_num > 0 {
            // SAFETY: the kernel reports desc_num descriptors at
            // desc_ptr, inside the reply region which is still mapped.
            let slice = unsafe {
                std::slice::from_raw_parts(
                    arg.desc_ptr as *const door_desc_t,
                    arg.desc_num as usize,
                )
            };
            for d in slice {
                // SAFETY: each entry is a descriptor the kernel just
                // installed in our table and will not touch again.
                received.push(unsafe { ReceivedFd::from_desc(d) });
            }
        }

        let map = mapped.then(|| Mapping {
            base: arg.rbuf as *mut c_void,
            len: arg.rsize,
        });

        // A client that opted out of descriptors closes anything that
        // arrives and says so. Dropping `received` closes them; this
        // happens before we return, and `map` is dropped with the
        // error, so nothing leaks on this path either.
        if !D::ACCEPTS && !received.is_empty() {
            let count = received.len();
            drop(received);
            drop(map);
            return Err(CallError::UnexpectedDescriptors { count });
        }

        // A caller-supplied buffer that overflowed: unmap now, so the
        // caller never sees a mapping (GOALS.md §6.2).
        if mapped && !supplied_rbuf.is_null() {
            let needed = arg.data_size;
            drop(received);
            drop(map);
            return Err(CallError::ReplyTooBig { needed });
        }

        Ok(Reply {
            _map: map,
            data: arg.data_ptr as *const u8,
            len: arg.data_size,
            descriptors: received,
        })
    }
}

impl Client<Descriptors> {
    /// Call the door, sending descriptors.
    ///
    /// Takes the descriptors **by value**, and it has to. The kernel
    /// closes a `DOOR_RELEASE` descriptor on almost every path, so a
    /// caller left holding an [`OwnedFd`] would close it a second
    /// time. Consuming them makes that unrepresentable.
    ///
    /// On [`CallError::Rejected`] — and only then — the descriptors
    /// come back in `returned`, because that is the one class of
    /// failure where the kernel did not take them.
    pub fn call_with_descriptors(
        &self,
        data: &[u8],
        fds: Vec<SentFd<'_>>,
    ) -> Result<Reply, CallError> {
        let reply = self.call_raw(data, fds, None)?;
        decode_tagged(reply)
    }
}

/// A view of a [`Client`] that does not use the §3.9 status tag.
///
/// Made by [`Client::untagged`]. It offers the same calls as `Client`,
/// and they behave the same in every way but one: the reply bytes are
/// **exactly** the bytes the server procedure produced. There is no
/// status byte and no framing of any kind.
///
/// Requests are never framed by this crate, in either mode. The bytes
/// you pass in are the bytes the server receives. Only the reply is
/// different here.
///
/// # There is no error channel
///
/// An untagged reply cannot say "this is an error". A C client would
/// not know how to read such a marker, so this crate does not write
/// one. On the server side that means:
///
/// - `Ok(bytes)` sends those bytes, exactly.
/// - `Err(e)` sends the [`ErrorReply`](crate::ErrorReply) encoding of
///   `e`, with **no marker**. The peer cannot tell it apart from a
///   success.
/// - A panic, or a cookie that no longer resolves, sends a reply of
///   **zero bytes**.
///
/// So if you need to report failure to a foreign peer, say so in your
/// own reply format. A C door server has to do the same thing.
///
/// # Errors you will never see
///
/// [`CallError::Server`] and [`CallError::ServerFailed`] cannot come
/// back from any method here. Both are built from the status tag, and
/// there is no tag to build them from.
#[derive(Debug)]
pub struct Untagged<'a, D> {
    client: &'a Client<D>,
}

impl<D: DescriptorPolicy> Untagged<'_, D> {
    /// Call the door and take the reply as it came.
    ///
    /// Like [`Client::call`], but [`Reply::data`] is exactly what the
    /// server sent. Nothing is skipped and nothing is parsed.
    ///
    /// Does **not** retry on `EINTR`; an interrupted call comes back as
    /// [`CallError::Interrupted`] because the server procedure may
    /// already have run and retrying would run it twice. Use
    /// [`call_idempotent`](Untagged::call_idempotent) when running
    /// twice is genuinely harmless.
    ///
    /// [`CallError::Server`] and [`CallError::ServerFailed`] are never
    /// returned: both come from the status tag.
    pub fn call(&self, data: &[u8]) -> Result<Reply, CallError> {
        self.client.call_raw(data, Vec::new(), None)
    }

    /// Call the door, retrying while it is interrupted.
    ///
    /// **An interrupted call may already have run.** `door_call`
    /// cannot tell you whether the server procedure completed before
    /// the interruption, so every retry risks executing it again. Only
    /// use this where a second execution is harmless.
    ///
    /// The reply is passed through untouched, as with
    /// [`call`](Untagged::call). [`CallError::Server`] and
    /// [`CallError::ServerFailed`] are never returned.
    pub fn call_idempotent(&self, data: &[u8]) -> Result<Reply, CallError> {
        loop {
            match self.client.call_raw(data, Vec::new(), None) {
                Err(CallError::Interrupted) => continue,
                other => return other,
            }
        }
    }

    /// Call the door, writing the reply into `buf`.
    ///
    /// Avoids a mapping entirely when the reply fits. When it does not,
    /// the kernel's overflow mapping is unmapped **before this
    /// returns**, and the caller gets [`CallError::ReplyTooBig`] with
    /// the size it would have needed. There is no path on which the
    /// caller observes a mapping here.
    ///
    /// The returned slice is the **whole** reply. The tagged version of
    /// this method drops the first byte, because that byte is the
    /// status tag; here there is no tag, so dropping a byte would eat
    /// real data.
    ///
    /// An empty slice is a normal answer, not an error: it is what an
    /// untagged server sends when its procedure panicked, and a C
    /// server may simply have nothing to say.
    ///
    /// [`CallError::Server`] and [`CallError::ServerFailed`] are never
    /// returned.
    pub fn call_into<'b>(
        &self,
        data: &[u8],
        buf: &'b mut [u8],
    ) -> Result<&'b [u8], CallError> {
        let base = buf.as_mut_ptr();
        let cap = buf.len();
        let reply =
            self.client.call_raw(data, Vec::new(), Some((base, cap)))?;

        // call_raw with a caller buffer never yields a mapping: it
        // turns overflow into ReplyTooBig before returning.
        debug_assert!(reply._map.is_none());

        // Answer a zero-length reply before touching the pointer. The
        // kernel is free to leave data_ptr null when there is nothing
        // to point at, and an empty reply is legal here.
        if reply.len == 0 {
            return Ok(&[]);
        }

        // The reply landed somewhere inside `buf`. Work out where, so
        // the slice we hand back covers all of it.
        let start = (reply.data as usize)
            .checked_sub(base as usize)
            .ok_or(CallError::Protocol("reply landed outside the buffer"))?;
        let len = reply.len;
        if start + len > cap {
            return Err(CallError::Protocol("reply overruns the buffer"));
        }

        Ok(&buf[start..start + len])
    }
}

impl Untagged<'_, Descriptors> {
    /// Call the door, sending descriptors, and take the reply as it
    /// came.
    ///
    /// Takes the descriptors **by value**, and it has to. The kernel
    /// closes a `DOOR_RELEASE` descriptor on almost every path, so a
    /// caller left holding an [`OwnedFd`] would close it a second
    /// time. Consuming them makes that unrepresentable.
    ///
    /// On [`CallError::Rejected`] — and only then — the descriptors
    /// come back in `returned`, because that is the one class of
    /// failure where the kernel did not take them.
    ///
    /// Only the reply framing differs from
    /// [`Client::call_with_descriptors`]: [`Reply::data`] is exactly
    /// what the server sent. [`CallError::Server`] and
    /// [`CallError::ServerFailed`] are never returned.
    pub fn call_with_descriptors(
        &self,
        data: &[u8],
        fds: Vec<SentFd<'_>>,
    ) -> Result<Reply, CallError> {
        self.client.call_raw(data, fds, None)
    }
}

/// Split the §3.9 status tag off the front of a reply.
fn decode_tagged(reply: Reply) -> Result<Reply, CallError> {
    let bytes = reply.data();
    let Some((&tag, _rest)) = bytes.split_first() else {
        return Err(CallError::Protocol("reply was empty; expected a tag"));
    };

    match StatusTag::from_byte(tag) {
        Some(StatusTag::Ok) => Ok(advance_one(reply)),
        Some(StatusTag::UserError) => Err(CallError::Server {
            data: bytes[1..].to_vec(),
        }),
        Some(StatusTag::Fault) => {
            let fault = bytes
                .get(1)
                .and_then(|b| ServerFault::from_byte(*b))
                .unwrap_or(ServerFault::Panicked);
            Err(CallError::ServerFailed(fault))
        }
        None => Err(CallError::Protocol("unrecognised status tag")),
    }
}

/// Step the payload past its status byte, keeping the mapping.
fn advance_one(mut reply: Reply) -> Reply {
    if reply.len > 0 {
        // SAFETY: len > 0, so data points at a readable byte and
        // data+1 is at most one past the end.
        reply.data = unsafe { reply.data.add(1) };
        reply.len -= 1;
    }
    reply
}

/// Decide what a `door_call` errno means for the descriptors.
///
/// `EFAULT` and `EBADF` mean the kernel rejected the call before
/// taking them, so they are ours to hand back. Everything else —
/// documented or not — means they are gone. Guessing generously here
/// would cause double closes, so the safe default is `Consumed`.
fn classify_call_failure(
    errno: doors_sys::Errno,
    released: Vec<RawFd>,
) -> CallError {
    match errno.get() {
        libc::EFAULT | libc::EBADF => {
            // Re-wrap: we still own these.
            let returned = released
                .into_iter()
                // SAFETY: these came from into_raw_fd and the kernel
                // did not take them, so we are the only owner.
                .map(|r| unsafe { OwnedFd::from_raw_fd(r) })
                .collect();
            CallError::Rejected { returned, errno }
        }
        libc::EINTR => {
            // Consumed. Do not close.
            drop(released);
            CallError::Interrupted
        }
        _ => {
            drop(released);
            CallError::Consumed(errno)
        }
    }
}

fn getparam(fd: RawFd, param: std::ffi::c_int) -> Result<usize, Error> {
    let mut out: usize = 0;
    // SAFETY: out is a live usize for the duration of the call.
    let rc = unsafe { sys::door_getparam(fd, param, &mut out) };
    if rc < 0 {
        return Err(Error::sys("door_getparam"));
    }
    Ok(out)
}
