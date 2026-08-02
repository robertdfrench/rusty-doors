// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Calling doors.

use crate::descriptor::{
    DescriptorPolicy, Descriptors, NoDescriptors, ReceivedFd, SentFd,
};
use crate::error::{
    CallError, Error, NotADoor, NotADoorReason, ServerFault, StatusTag,
};
use crate::server::DoorInfo;
use crate::sys;
use crate::types::{door_arg_t, door_desc_t};
use doors_sys::{
    DOOR_PARAM_DATA_MAX, DOOR_PARAM_DATA_MIN, DOOR_PARAM_DESC_MAX,
    DOOR_REFUSE_DESC,
};
use std::ffi::{c_char, c_void, CString};
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

/// A mapping the kernel made for a reply, unmapped on `Drop`.
///
/// Its own type, with its own `Drop`, so that unmapping does not
/// depend on any other field of [`Reply`] being in a particular state.
/// [`Reply`] must always unmap, and the simplest way to always is to
/// make it the only thing this type does.
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
/// nor `IntoRawFd`, because handing it out would let someone `close`
/// it or `door_call` it behind the typestate's back.
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
    /// Inheriting a door is the exception, so it has to be asked for
    /// by name. Prefer [`open`](Client::open) unless a child program
    /// genuinely needs to inherit this door.
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
    ///
    /// # It governs both directions
    ///
    /// The name points at sending. The consequence points the other
    /// way as well: this is also the only way to get a client that can
    /// **receive** a descriptor. A [`Client<NoDescriptors>`](Client)
    /// that is handed one closes it and fails the call with
    /// [`CallError::UnexpectedDescriptors`].
    ///
    /// So call this whenever the door may reply with a descriptor,
    /// even if you never send one yourself.
    ///
    /// The same coin, from the server side: a door built with
    /// [`refuse_descriptors`] can never return a descriptor to
    /// anybody, because this method refuses such a door. A door that
    /// wants to turn away incoming descriptors and still reply with
    /// one should use [`max_descriptors(0)`] instead.
    ///
    /// [`refuse_descriptors`]: crate::DoorBuilder::refuse_descriptors
    /// [`max_descriptors(0)`]: crate::DoorBuilder::max_descriptors
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

    /// Take ownership of a door that arrived in a request or a reply.
    ///
    /// A door that is passed between processes arrives as a
    /// descriptor. There is no path for it, so [`open`](Client::open)
    /// cannot be used. This is the way in for that door.
    ///
    /// ```no_run
    /// # use doors::{Client, Reply};
    /// # fn demo(reply: Reply) -> Result<(), Box<dyn std::error::Error>> {
    /// let arrived = reply.into_descriptors().pop().expect("a door");
    /// let client = Client::from_received(arrived)?;
    /// let answer = client.call(b"hello")?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # It costs one system call, and it has to
    ///
    /// `door_info(3C)`. The attributes the kernel delivered alongside
    /// the descriptor look like they should answer this, and they do
    /// not.
    ///
    /// `DOOR_DESCRIPTOR` is set on **everything** a door call
    /// delivers: files, pipes, sockets, doors. It means "a descriptor
    /// is being passed here", not "this is a door". The other bits are
    /// the door's own flags, and they are copied in only for a door —
    /// so a door made with no flags in another process arrives looking
    /// exactly like a pipe. Measured; see
    /// `doors/tests/adopt_a_door.rs`, which sends a plain file through
    /// a door call and finds `DOOR_DESCRIPTOR` set on it.
    ///
    /// So there is nothing to read, and this asks.
    ///
    /// # To send or receive descriptors on it
    ///
    /// Add [`with_descriptors`](Client::with_descriptors):
    ///
    /// ```no_run
    /// # use doors::{Client, Descriptors, ReceivedFd};
    /// # fn demo(arrived: ReceivedFd)
    /// #     -> Result<(), Box<dyn std::error::Error>> {
    /// let client: Client<Descriptors> =
    ///     Client::from_received(arrived)?.with_descriptors()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Or, in one step and one system call,
    /// [`MaybeDoor::into_client_with_descriptors`]:
    ///
    /// ```no_run
    /// # use doors::{Client, Descriptors, MaybeDoor, ReceivedFd};
    /// # fn demo(arrived: ReceivedFd)
    /// #     -> Result<(), Box<dyn std::error::Error>> {
    /// let client = MaybeDoor::new(arrived.into_owned())
    ///     .into_client_with_descriptors()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # The descriptor's flags are left alone
    ///
    /// [`open`](Client::open) sets `FD_CLOEXEC`, because it creates
    /// the descriptor and so gets to choose. This does not create
    /// anything; it adopts a descriptor that already exists and
    /// already has flags somebody else chose. Changing them quietly
    /// would be a surprise. Set them yourself if you need to.
    ///
    /// # Errors
    ///
    /// [`NotADoor`] when the descriptor is not a door, or is a door
    /// that has been revoked. The descriptor comes back in the error,
    /// still open.
    pub fn from_received(fd: ReceivedFd) -> Result<Self, NotADoor> {
        let fd = fd.into_owned();
        match vet(fd.as_raw_fd(), false) {
            Ok(()) => Ok(Client {
                fd,
                _d: PhantomData,
            }),
            Err(reason) => Err(NotADoor { fd, reason }),
        }
    }

    /// Adopt a descriptor as a door, asking the kernel nothing.
    ///
    /// For a caller who already knows what they have and would rather
    /// not pay for the `door_info(3C)` call that
    /// [`MaybeDoor::into_client`] makes.
    ///
    /// # This is not `unsafe`, and here is why
    ///
    /// You hand over an [`OwnedFd`], so ownership is already settled
    /// and nothing here can close a descriptor twice. If the promise
    /// below is broken, the result is a wrong answer, not broken
    /// memory:
    ///
    /// - **Not a door.** Every call fails with
    ///   [`CallError::Rejected`] carrying `EBADF`, and the `returned`
    ///   list is empty because a plain call sends nothing. Nothing is
    ///   read, nothing is written, no memory is touched. Measured; see
    ///   `doors/tests/adopt_a_door.rs`.
    /// - **A revoked door.** The same, with `EBADF`.
    ///
    /// Rust reserves `unsafe` for what can break memory safety. This
    /// cannot, so it is not marked `unsafe`. The `_unchecked` name is
    /// the warning instead. Compare
    /// [`MaybeDoor::from_raw_fd`](MaybeDoor::from_raw_fd), which **is**
    /// `unsafe`: a [`RawFd`] carries no ownership, and taking one on
    /// trust really can lead to a double close.
    ///
    /// # What you are promising
    ///
    /// That `fd` is an open, unrevoked door. Nothing checks it.
    ///
    /// # Name the state
    ///
    /// Write `Client::<NoDescriptors>::from_fd_unchecked(fd)`.
    /// [`Client<Descriptors>`](Client) has a method of the same name,
    /// so a bare `Client::from_fd_unchecked(fd)` does not compile.
    pub fn from_fd_unchecked(fd: OwnedFd) -> Self {
        Client {
            fd,
            _d: PhantomData,
        }
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
    /// Adopt a descriptor as a door that carries descriptors, asking
    /// the kernel nothing.
    ///
    /// This is the one that saves real work. Every other way to a
    /// [`Client<Descriptors>`](Client) makes a `door_info(3C)` call to
    /// see whether the door was created with `DOOR_REFUSE_DESC`. That
    /// is one system call per door adopted, and a program that adopts
    /// doors at a high rate pays it every time. This skips it.
    ///
    /// ```no_run
    /// # use doors::{Client, Descriptors};
    /// # fn demo(fd: std::os::fd::OwnedFd) {
    /// // We created this door ourselves and know it takes descriptors.
    /// let client = Client::<Descriptors>::from_fd_unchecked(fd);
    /// # }
    /// ```
    ///
    /// # Name the state, always
    ///
    /// Write `Client::<Descriptors>::from_fd_unchecked(fd)`, with the
    /// turbofish. There is a method of this name on
    /// [`Client<NoDescriptors>`](Client) too, and a bare
    /// `Client::from_fd_unchecked(fd)` does not compile: the compiler
    /// cannot tell which one you meant, even from the type you are
    /// assigning to. Saying it out loud is no bad thing for a method
    /// that skips a check.
    ///
    /// # This is not `unsafe`, and here is why
    ///
    /// You hand over an [`OwnedFd`], so ownership is settled and
    /// nothing here can close a descriptor twice. Breaking the promise
    /// gives wrong answers, not broken memory. See
    /// [`Client::<NoDescriptors>::from_fd_unchecked`] for the longer
    /// version of that argument.
    ///
    /// # What you are promising
    ///
    /// Two things, and the second one costs the most to get wrong.
    ///
    /// 1. That `fd` is an open, unrevoked door. If it is not, every
    ///    call fails with [`CallError::Rejected`] carrying `EBADF`.
    ///    Nothing else happens.
    ///
    /// 2. That the door was **not** created with `DOOR_REFUSE_DESC`.
    ///
    /// # If you get the second one wrong
    ///
    /// [`call_with_descriptors`](Client::call_with_descriptors) fails
    /// with [`CallError::Rejected`] carrying `ENOTSUP`, and your
    /// descriptors come back in `returned`. The kernel refuses a
    /// descriptor-carrying call to such a door before taking
    /// anything, so nothing is lost — you just do not get a reply.
    ///
    /// This used to leak one descriptor per call, because `ENOTSUP`
    /// fell through to [`CallError::Consumed`], which says the kernel
    /// took them. `experiments/refuse_desc_errno.c` measured that it
    /// does not.
    ///
    /// Measured, both parts; see `doors/tests/adopt_a_door.rs`.
    ///
    /// That is the whole argument for the check this method skips. It
    /// is one `door_info(3C)` call, once, at adoption, and it turns a
    /// silent per-call leak into one error at the point of the
    /// mistake. Skip it only for a door you created yourself, or one
    /// whose flags you have already read.
    ///
    /// [`Client::<NoDescriptors>::from_fd_unchecked`]:
    ///     Client::from_fd_unchecked
    pub fn from_fd_unchecked(fd: OwnedFd) -> Self {
        Client {
            fd,
            _d: PhantomData,
        }
    }

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

// ---------------------------------------------------------------------
// Adopting a descriptor that is supposed to be a door
// ---------------------------------------------------------------------

/// Ask the kernel whether this descriptor is a door we can use.
///
/// `door_info(3C)` is the test, and it is a good one: it answers for a
/// door and fails with `EBADF` for anything else. There is no other
/// way to ask. Nothing about a descriptor's number says what is behind
/// it.
///
/// `wants_descriptors` says whether the caller is building a
/// [`Client<Descriptors>`](Client). `DOOR_REFUSE_DESC` only matters
/// then; a door that refuses descriptors is a fine door for plain
/// calls.
fn vet(fd: RawFd, wants_descriptors: bool) -> Result<(), NotADoorReason> {
    let info =
        crate::server::door_info_errno(fd).map_err(NotADoorReason::NotADoor)?;

    // A revoked door answers nothing, so a client over one could only
    // ever fail. Failing here is better: the caller still has the
    // descriptor and can do something else with it.
    if info.is_revoked() {
        return Err(NotADoorReason::Revoked);
    }

    if wants_descriptors && info.refuses_descriptors() {
        return Err(NotADoorReason::RefusesDescriptors);
    }

    Ok(())
}

/// A descriptor that is supposed to be a door. Nobody has checked yet.
///
/// A descriptor can arrive from anywhere: inherited across an `exec`,
/// passed over a UNIX socket, named on the command line. Nothing about
/// the number says what is behind it. This type is that doubt, written
/// down.
///
/// It exists so the check cannot be skipped by accident. There is no
/// way from here to a [`Client`] that does not go past the check.
/// (There is a way that skips it on purpose —
/// [`Client::from_fd_unchecked`] — but you have to name it.)
///
/// ```no_run
/// # use doors::{Client, MaybeDoor};
/// # use std::os::fd::OwnedFd;
/// # fn demo(inherited: OwnedFd)
/// #     -> Result<(), Box<dyn std::error::Error>> {
/// let client = MaybeDoor::new(inherited).into_client()?;
/// let reply = client.call(b"hello")?;
/// # Ok(())
/// # }
/// ```
///
/// # For a descriptor that came from a door call
///
/// [`Client::from_received`] is the friendlier way in: it takes a
/// [`ReceivedFd`] and does the same check. It is not cheaper. The
/// attributes the kernel delivers cannot tell a door from a pipe, so
/// both go and ask.
///
/// # If it is not a door
///
/// The descriptor comes back in the error. See [`NotADoor`].
///
/// # The descriptor's flags are left alone
///
/// Nothing here sets or clears `FD_CLOEXEC`. The descriptor already
/// existed and its flags are somebody else's choice. Only
/// [`Client::open`], which creates the descriptor, chooses for you.
///
/// # Why it is called that
///
/// `Maybe` is the standard library's own word for a value whose
/// contents are not yet established: `MaybeUninit`, `MaybeDangling`,
/// `MaybeDone`. It is the only prefix `std` uses for this, so it is
/// the one used here.
///
/// For a value that is *supposed* to be something, `std` would more
/// often skip the wrapper and offer a fallible constructor —
/// `str::from_utf8` takes bytes that are supposed to be UTF-8 and
/// answers with a `Result`. That shape is available here too, as
/// [`Client::from_fd_unchecked`] and [`Client::from_received`].
///
/// This type earns its place by holding the descriptor while you
/// decide *which* client you want. A constructor on `Client` cannot,
/// because the same name would live on both `Client<NoDescriptors>`
/// and `Client<Descriptors>` and every call would need a turbofish.
#[derive(Debug)]
pub struct MaybeDoor {
    fd: OwnedFd,
}

impl MaybeDoor {
    /// Take a descriptor that might be a door.
    pub fn new(fd: OwnedFd) -> Self {
        MaybeDoor { fd }
    }

    /// Take a raw descriptor that might be a door.
    ///
    /// # Safety
    ///
    /// This one really is `unsafe`, unlike the `_unchecked`
    /// constructors elsewhere in this module. The reason is ownership,
    /// not doors.
    ///
    /// The caller must guarantee:
    ///
    /// - `fd` is open, and
    /// - the caller owns it, and gives that ownership up here.
    ///
    /// A [`RawFd`] is a plain integer. It says nothing about who is
    /// responsible for closing it, and there is no way to find out. If
    /// something else also owns this descriptor, both owners will
    /// close it. The second close may land on a completely unrelated
    /// file that has since taken the same number, and then reads and
    /// writes meant for one file go to another. That is why this is
    /// `unsafe` and [`new`](MaybeDoor::new) is not: an [`OwnedFd`]
    /// carries the ownership the caller has to promise here.
    ///
    /// Whether `fd` is a door is a separate question, and not a safety
    /// one. [`into_client`](MaybeDoor::into_client) answers it.
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        MaybeDoor {
            // SAFETY: the caller promised this descriptor is open and
            // that they are handing over their ownership of it.
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        }
    }

    /// Ask the kernel about it, without giving up ownership.
    ///
    /// Useful before deciding what to do: the answer says which
    /// process serves the door, whether it is revoked, and whether it
    /// refuses descriptors. Fails with [`Error::Sys`] when the
    /// descriptor is not a door at all.
    ///
    /// This is the same call the conversions make, so a caller who
    /// only wants a client should not call it first — just convert,
    /// and read the reason out of the error.
    pub fn inspect(&self) -> Result<DoorInfo, Error> {
        crate::server::door_info_for(self.fd.as_raw_fd())
    }

    /// Check it, and on success make a client for plain calls.
    ///
    /// # Errors
    ///
    /// [`NotADoor`] when the descriptor is not a door, or is a door
    /// that has been revoked. Your descriptor comes back in the error.
    pub fn into_client(self) -> Result<Client<NoDescriptors>, NotADoor> {
        match vet(self.fd.as_raw_fd(), false) {
            Ok(()) => Ok(Client {
                fd: self.fd,
                _d: PhantomData,
            }),
            Err(reason) => Err(NotADoor {
                fd: self.fd,
                reason,
            }),
        }
    }

    /// Check it, and on success make a client that carries
    /// descriptors.
    ///
    /// One extra check on top of [`into_client`](MaybeDoor::into_client):
    /// a door created with `DOOR_REFUSE_DESC` is refused, because such
    /// a door can neither take a descriptor nor send one back. A
    /// client over it could only ever fail.
    ///
    /// # Errors
    ///
    /// [`NotADoor`], with [`NotADoorReason::RefusesDescriptors`] for
    /// that last case. Your descriptor comes back in the error.
    pub fn into_client_with_descriptors(
        self,
    ) -> Result<Client<Descriptors>, NotADoor> {
        match vet(self.fd.as_raw_fd(), true) {
            Ok(()) => Ok(Client {
                fd: self.fd,
                _d: PhantomData,
            }),
            Err(reason) => Err(NotADoor {
                fd: self.fd,
                reason,
            }),
        }
    }

    /// Take the descriptor back.
    ///
    /// Nothing was done to it, so this gives back exactly what
    /// [`new`](MaybeDoor::new) was given.
    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

/// A [`Client`] that borrows its descriptor instead of owning it.
///
/// For calling a door many times without taking it over. The common
/// case is a descriptor that belongs to something else and has to keep
/// belonging to it: one held by a [`Reply`], or by a
/// [`ReceivedFd`](crate::ReceivedFd) you want to hand on afterwards.
///
/// ```no_run
/// # use doors::{BorrowedClient, ReceivedFd};
/// # fn demo(arrived: &ReceivedFd)
/// #     -> Result<(), Box<dyn std::error::Error>> {
/// let door = BorrowedClient::new(arrived.as_fd())?;
/// for _ in 0..1000 {
///     let reply = door.call(b"tick")?;
/// }
/// // `arrived` still owns the descriptor and still closes it.
/// # Ok(())
/// # }
/// ```
///
/// # It has every method a `Client` has
///
/// Through [`Deref`]. Every calling method on [`Client`] takes
/// `&self`, so `borrowed.call(..)`, `borrowed.call_into(..)`,
/// `borrowed.untagged()`, `borrowed.info()` and `borrowed.limits()`
/// all work, and a `BorrowedClient<'_, Descriptors>` also has
/// [`call_with_descriptors`](Client::call_with_descriptors). Nothing
/// is duplicated here, and [`Client`] itself is unchanged.
///
/// There is deliberately no `DerefMut`, and no way to get an owned
/// [`Client`] out. Either one would let the descriptor be closed by
/// something that does not own it.
///
/// # It never closes the descriptor
///
/// The whole point. The lifetime is borrowed from the descriptor, so
/// the borrow checker will not let this outlive whatever does own it.
#[derive(Debug)]
pub struct BorrowedClient<'a, D = NoDescriptors> {
    /// Never dropped, so the `OwnedFd` inside is never closed.
    inner: ManuallyDrop<Client<D>>,
    /// Carries the borrow, so this cannot outlive the descriptor.
    _fd: PhantomData<BorrowedFd<'a>>,
}

impl<'a, D> BorrowedClient<'a, D> {
    /// Wrap a borrowed descriptor without taking it over.
    fn wrap(fd: BorrowedFd<'a>) -> Self {
        let client = Client {
            // SAFETY: this OwnedFd must never close what it holds,
            // because we do not own it. ManuallyDrop is what
            // guarantees that: the Client is never dropped, so its
            // OwnedFd is never dropped either, so the descriptor is
            // never closed. Nothing else in this type touches it.
            fd: unsafe { OwnedFd::from_raw_fd(fd.as_raw_fd()) },
            _d: PhantomData,
        };
        BorrowedClient {
            inner: ManuallyDrop::new(client),
            _fd: PhantomData,
        }
    }
}

impl<'a> BorrowedClient<'a, NoDescriptors> {
    /// Borrow a door for plain calls, checking that it is one.
    ///
    /// Costs one `door_info(3C)` call, once, not once per door call.
    ///
    /// # Errors
    ///
    /// [`NotADoorReason`] on its own, and not [`NotADoor`]: there is
    /// no descriptor to hand back, because you never gave one up.
    pub fn new(fd: BorrowedFd<'a>) -> Result<Self, NotADoorReason> {
        vet(fd.as_raw_fd(), false)?;
        Ok(Self::wrap(fd))
    }

    /// Borrow a door for plain calls, asking the kernel nothing.
    ///
    /// Not `unsafe`: the descriptor is borrowed, so its ownership is
    /// already settled, and a descriptor that turns out not to be a
    /// door only makes every call fail with `EBADF`. See
    /// [`Client::from_fd_unchecked`] for the full argument.
    pub fn new_unchecked(fd: BorrowedFd<'a>) -> Self {
        Self::wrap(fd)
    }
}

impl<'a> BorrowedClient<'a, Descriptors> {
    /// Borrow a door that carries descriptors, checking that it can.
    ///
    /// Refuses a door created with `DOOR_REFUSE_DESC`, which can
    /// neither take a descriptor nor send one back.
    ///
    /// # Errors
    ///
    /// [`NotADoorReason`] on its own. There is no descriptor to hand
    /// back, because you never gave one up.
    pub fn with_descriptors(
        fd: BorrowedFd<'a>,
    ) -> Result<Self, NotADoorReason> {
        vet(fd.as_raw_fd(), true)?;
        Ok(Self::wrap(fd))
    }

    /// Borrow a door that carries descriptors, asking the kernel
    /// nothing.
    ///
    /// Not `unsafe`, for the reasons given on
    /// [`Client::<Descriptors>::from_fd_unchecked`]. You are promising
    /// the same two things: that this is a live door, and that it was
    /// not created with `DOOR_REFUSE_DESC`.
    ///
    /// [`Client::<Descriptors>::from_fd_unchecked`]:
    ///     Client::from_fd_unchecked
    pub fn with_descriptors_unchecked(fd: BorrowedFd<'a>) -> Self {
        Self::wrap(fd)
    }
}

impl<D> Deref for BorrowedClient<'_, D> {
    type Target = Client<D>;

    fn deref(&self) -> &Client<D> {
        &self.inner
    }
}

/// A view of a [`Client`] that does not read a status byte.
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

/// Split the status byte off the front of a reply.
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
///
/// `ENOTSUP` was added to the hand-back list after measuring it.
/// Sending a descriptor to a door created with `DOOR_REFUSE_DESC`
/// fails with `ENOTSUP`, and the kernel rejects the call *before*
/// taking anything — `experiments/refuse_desc_errno.c` shows the
/// descriptor still open afterwards and still referring to the same
/// file, comparing `st_dev`/`st_ino`/`st_rdev` rather than trusting
/// `F_GETFD`. Treating it as consumed leaked one descriptor per call.
///
/// Note the shape of that evidence. A descriptor is only added to this
/// list on a measurement that checks file identity; a descriptor
/// *number* can be closed and handed straight back out, so
/// `fcntl(F_GETFD)` succeeding proves nothing on its own.
fn classify_call_failure(
    errno: doors_sys::Errno,
    released: Vec<RawFd>,
) -> CallError {
    match errno.get() {
        libc::EFAULT | libc::EBADF | libc::ENOTSUP => {
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
