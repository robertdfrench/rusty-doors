// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Building a door.

use crate::error::Error;
use crate::registry::{self, DoorInner};
use crate::server::cookie;
use crate::server::trampoline::ReplyProtocol;
use crate::server::Door;
use crate::sys;
use crate::types::{door_info_t, ServerProcedure};
use doors_sys::{
    DOOR_NO_CANCEL, DOOR_PARAM_DATA_MAX, DOOR_PARAM_DATA_MIN,
    DOOR_PARAM_DESC_MAX, PTHREAD_CANCEL_DISABLE,
};
use std::ffi::{c_int, c_uint};
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex, Once};

/// The default stack for a door server thread.
///
/// Request data lands on this stack, so it has to be comfortably
/// larger than the largest request the door accepts.
pub const DEFAULT_THREAD_STACK: usize = 256 * 1024;

/// Headroom left for the server procedure itself, on top of whatever
/// the request needs.
///
/// The kernel puts the request data, the descriptor array and a
/// `door_info_t` on the server thread stack before the procedure ever
/// runs. This is the space the procedure gets after all that.
const STACK_SLACK: usize = 64 * 1024;

/// Assemble a door, then create it.
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # use doors::Door;
/// # extern "C" fn my_proc(
/// #     _: *mut std::ffi::c_void, _: *mut std::ffi::c_char,
/// #     _: usize, _: *mut std::ffi::c_void, _: u32) {}
/// let door = Door::builder(String::from("state"))
///     .request_size(0..=64 * 1024)
///     .max_descriptors(0)
///     .thread_stack_size(256 * 1024)
///     .build(unsafe { std::mem::transmute::<_, _>(my_proc as *const ()) })?;
/// # Ok(())
/// # }
/// ```
///
/// Most users never call [`build`](DoorBuilder::build) directly. The
/// `#[doors::server]` macro generates a `build_<method>()` that takes
/// no server procedure argument, because it already knows which one to
/// use.
pub struct DoorBuilder<S>
where
    S: Send + Sync + 'static,
{
    state: S,
    attributes: c_uint,
    data_range: Option<RangeInclusive<usize>>,
    desc_max: Option<usize>,
    stack_size: usize,
    /// How replies are framed. See [`untagged`](DoorBuilder::untagged).
    protocol: ReplyProtocol,
    /// Set when the caller used a builder method the `#[door(...)]`
    /// attribute also sets, so `build_foo()` can refuse rather than
    /// silently pick one.
    pub(crate) explicit_request_size: bool,
    pub(crate) explicit_max_descriptors: bool,
}

impl<S: Send + Sync + 'static> DoorBuilder<S> {
    pub(crate) fn new(state: S) -> Self {
        DoorBuilder {
            state,
            // DOOR_NO_CANCEL is set here, once, and there is no method
            // to clear it. GOALS.md §12.7. A cancelled server thread
            // leaves the door's state half-updated with no way to tell.
            attributes: DOOR_NO_CANCEL,
            data_range: None,
            desc_max: None,
            stack_size: DEFAULT_THREAD_STACK,
            // Tagged by default: two peers that both use this crate
            // get the richer errors unless someone asks otherwise.
            protocol: ReplyProtocol::Tagged,
            explicit_request_size: false,
            explicit_max_descriptors: false,
        }
    }

    /// The smallest and largest request this door accepts, in bytes.
    ///
    /// Sets `DOOR_PARAM_DATA_MIN` and `DOOR_PARAM_DATA_MAX`. The
    /// maximum also decides how big the server thread stack has to be.
    pub fn request_size(mut self, range: RangeInclusive<usize>) -> Self {
        self.data_range = Some(range);
        self.explicit_request_size = true;
        self
    }

    /// The most descriptors a caller may send per call.
    ///
    /// Sets `DOOR_PARAM_DESC_MAX`. Zero means none, which is not the
    /// same as refusing them: see
    /// [`refuse_descriptors`](DoorBuilder::refuse_descriptors).
    pub fn max_descriptors(mut self, n: usize) -> Self {
        self.desc_max = Some(n);
        self.explicit_max_descriptors = true;
        self
    }

    /// Refuse descriptors outright, with `DOOR_REFUSE_DESC`.
    ///
    /// Stronger than a maximum of zero. The kernel rejects the call
    /// before the server procedure runs, and a client can see the
    /// refusal in `door_info` before it even tries.
    pub fn refuse_descriptors(mut self) -> Self {
        self.attributes |= doors_sys::DOOR_REFUSE_DESC;
        self
    }

    /// Reply with no status byte, for a caller that does not use this
    /// crate.
    ///
    /// By default every reply starts with one byte saying whether the
    /// server procedure returned data, returned an error, or failed
    /// (`GOALS.md` §3.9). That byte is a private agreement between two
    /// peers that both use this crate. A door client written in C
    /// knows nothing about it and would read it as the first byte of
    /// the reply. So this switches it off: the reply is then exactly
    /// the bytes the server procedure produced, with no framing of any
    /// kind.
    ///
    /// Requests are untouched either way. This crate never adds
    /// anything to a request and never strips anything off one.
    ///
    /// # An untagged reply cannot report an error
    ///
    /// There is no room left to say "this is an error", so:
    ///
    /// * `Ok(bytes)` sends those bytes, exactly.
    /// * `Err(e)` sends the [`ErrorReply`](crate::ErrorReply) encoding
    ///   of `e` with **no marker**. The caller cannot tell it apart
    ///   from a success. If your caller needs to know about failures,
    ///   put that in your own reply format, exactly as a C door server
    ///   would.
    /// * A panic, or a cookie that no longer resolves, sends a reply
    ///   of zero bytes.
    ///
    /// # Which server procedures this reaches
    ///
    /// A generated `build_<method>()` reads this setting and registers
    /// the entry point that matches, so `.untagged().build_hello()`
    /// does what it says. `#[door(untagged)]` on the method sets the
    /// same thing.
    ///
    /// [`build`](DoorBuilder::build) is the exception. A hand-written
    /// server procedure sends its own reply, and nothing here can
    /// frame bytes this crate never sees. Pass
    /// [`ReplyProtocol::Untagged`] to
    /// [`run`](crate::server::trampoline::run) yourself, or call
    /// `door_return` directly.
    pub fn untagged(mut self) -> Self {
        self.protocol = ReplyProtocol::Untagged;
        self
    }

    /// How this door frames its replies. Read by generated code.
    pub(crate) fn reply_protocol(&self) -> ReplyProtocol {
        self.protocol
    }

    /// Ask for an unreferenced notification.
    pub fn unref(mut self) -> Self {
        self.attributes |= doors_sys::DOOR_UNREF;
        self
    }

    /// Ask for repeated unreferenced notifications.
    pub fn unref_multi(mut self) -> Self {
        self.attributes |= doors_sys::DOOR_UNREF_MULTI;
        self
    }

    /// Give this door its own pool of server threads.
    pub fn private_pool(mut self) -> Self {
        self.attributes |= doors_sys::DOOR_PRIVATE;
        self
    }

    /// How much stack each server thread gets.
    ///
    /// Must be large enough for the largest request plus the
    /// procedure's own needs; [`build`](DoorBuilder::build) checks and
    /// refuses if not.
    pub fn thread_stack_size(mut self, bytes: usize) -> Self {
        self.stack_size = bytes;
        self
    }

    /// The smallest stack that could work for the declared limits.
    fn stack_needed(&self) -> usize {
        let data = self.data_range.as_ref().map_or(0, |r| *r.end());
        let descs = self.desc_max.unwrap_or(0)
            * std::mem::size_of::<crate::types::door_desc_t>();
        data + descs + std::mem::size_of::<door_info_t>() + STACK_SLACK
    }

    /// Create the door.
    ///
    /// `server_procedure` is the raw `extern "C"` entry point. Use the
    /// generated `build_<method>()` instead wherever you can; this is
    /// for hand-written server procedures.
    pub fn build(
        self,
        server_procedure: ServerProcedure,
    ) -> Result<Door<S>, Error> {
        // The stack check is real, not a formality: request data,
        // descriptors and door_info_t all land on the server thread
        // stack before the procedure runs.
        let needed = self.stack_needed();
        if self.stack_size < needed {
            return Err(Error::StackTooSmall {
                requested: self.stack_size,
                needed,
            });
        }

        // SAFETY: no arguments; returns a size the library computed.
        let floor = unsafe { sys::thr_min_stack() };
        if self.stack_size < floor {
            return Err(Error::StackTooSmall {
                requested: self.stack_size,
                needed: floor,
            });
        }

        // The atfork backstop must exist before any door does, or a
        // fork between creating the door and registering the handlers
        // would leave the child holding a live server door.
        registry::install_atfork_once();

        let DoorBuilder {
            state,
            attributes,
            data_range,
            desc_max,
            stack_size,
            ..
        } = self;

        let (handle, cookie) = cookie::install(Arc::new(state));

        // Record the stack size before creating the door, because the
        // very first server thread can be made during door_create
        // itself. The cookie is the only thing we know at that point
        // that also reaches the thread-creation callback.
        register_stack_size(cookie as usize, stack_size);
        install_server_create_once();

        // SAFETY: server_procedure is a valid entry point and cookie
        // is the slot number the kernel hands back on every call.
        let fd =
            unsafe { sys::door_create(server_procedure, cookie, attributes) };
        if fd < 0 {
            let err = Error::sys("door_create");
            forget_stack_size(cookie as usize);
            // Do not leak the state if the door could not be made.
            cookie::uninstall(handle);
            return Err(err);
        }

        // Parameters have to be set after creation. A failure here
        // leaves a live door, so unwind it properly.
        let set = |param: c_int, value: usize| -> Result<(), Error> {
            // SAFETY: fd is the door we just created.
            let rc = unsafe { sys::door_setparam(fd, param, value) };
            if rc < 0 {
                Err(Error::sys("door_setparam"))
            } else {
                Ok(())
            }
        };

        let params = (|| -> Result<(), Error> {
            if let Some(range) = &data_range {
                set(DOOR_PARAM_DATA_MIN, *range.start())?;
                set(DOOR_PARAM_DATA_MAX, *range.end())?;
            }
            if let Some(n) = desc_max {
                set(DOOR_PARAM_DESC_MAX, n)?;
            }
            Ok(())
        })();

        if let Err(e) = params {
            // The door exists but is not registered yet, so nothing
            // else can see it. One release, and one only:
            // `door_revoke` closes the descriptor itself, so there
            // must be no `close` after it. A second close here would
            // shut whatever descriptor number another thread had just
            // been handed (`docs/DESIGN.md` Appendix E).
            //
            // SAFETY: fd is the door we just created and still open.
            unsafe {
                sys::door_revoke(fd);
            }
            cookie::uninstall(handle);
            return Err(e);
        }

        let inner = DoorInner::new(fd);
        let key = registry::register(inner.clone());
        Ok(Door::from_parts(inner, key, handle))
    }
}

/// Per-door server thread stack sizes, keyed by cookie.
///
/// `door_server_create(3C)` installs ONE thread-creation function for
/// the whole process, but each door may want a different stack. The
/// callback is handed a `door_info_t`, whose `di_data` field is the
/// door's cookie — the one value we already know before `door_create`
/// returns. So the cookie is the key.
static STACK_SIZES: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

fn register_stack_size(cookie: usize, stack: usize) {
    let mut table = STACK_SIZES.lock().unwrap_or_else(|e| e.into_inner());
    match table.iter_mut().find(|(c, _)| *c == cookie) {
        Some(entry) => entry.1 = stack,
        None => table.push((cookie, stack)),
    }
}

fn forget_stack_size(cookie: usize) {
    let mut table = STACK_SIZES.lock().unwrap_or_else(|e| e.into_inner());
    table.retain(|(c, _)| *c != cookie);
}

fn stack_size_for(cookie: usize) -> usize {
    let table = STACK_SIZES.lock().unwrap_or_else(|e| e.into_inner());
    table
        .iter()
        .find(|(c, _)| *c == cookie)
        .map(|(_, s)| *s)
        .unwrap_or(DEFAULT_THREAD_STACK)
}

/// Install our thread-creation function, once per process.
fn install_server_create_once() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: create_server_thread has the signature
        // door_server_create expects and stays valid for the life of
        // the process.
        unsafe {
            sys::door_server_create(Some(create_server_thread));
        }
    });
}

/// Make one door server thread.
///
/// # Why not `door_xcreate`?
///
/// `door_xcreate(3C)` looks like the right tool: it takes a
/// thread-creation function per door, so there would be no table to
/// keep. It does not work on OmniOS r151058.
///
/// The argument it hands the creation function points into
/// `door_xcreate`'s own stack frame. If the creation function starts a
/// thread and returns — the obvious implementation — that frame is
/// gone before the new thread reads it, and libc dereferences whatever
/// replaced it. It does not fail; it segfaults inside
/// `privdoor_data_hold`. Synchronising so the new thread copies the
/// argument first avoids the crash, and then `door_xcreate` returns
/// `EINVAL` instead. See `experiments/xcreate3.c`.
///
/// `door_create` plus `door_server_create` is the older, documented
/// mechanism, and it does exactly what is needed: the thread below
/// really does get the stack size that was asked for, which
/// `experiments/servercreate.c` confirms by reading it back with
/// `thr_stksegment`.
unsafe extern "C" fn create_server_thread(info: *mut door_info_t) {
    // Copy the field out. door_info_t is packed, so a reference to
    // di_data would be misaligned.
    let cookie = if info.is_null() {
        0
    } else {
        (*info).di_data as usize
    };
    let stack = stack_size_for(cookie);

    let _ = std::thread::Builder::new()
        .stack_size(stack)
        .name(String::from("door-server"))
        .spawn(move || {
            // First thing on every server thread. A cancelled server
            // thread would abandon whatever the procedure was in the
            // middle of. GOALS.md §5.5.
            // SAFETY: a constant and a null pointer.
            unsafe {
                sys::pthread_setcancelstate(
                    PTHREAD_CANCEL_DISABLE,
                    std::ptr::null_mut(),
                );
            }

            // Join the door's pool of waiting threads. This does not
            // return: from here the kernel drives the thread, entering
            // the server procedure when a call arrives.
            //
            // Because it never returns, nothing after it runs and no
            // destructor on this frame is ever called. That is why
            // there is nothing here to destruct.
            // SAFETY: the null/zero form is how a thread makes itself
            // available; see door_return(3C).
            unsafe {
                sys::door_return(std::ptr::null(), 0, std::ptr::null(), 0);
            }
        });
}
