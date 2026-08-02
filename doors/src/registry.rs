// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! What happens to a server door when the process forks.
//!
//! # The problem
//!
//! `fork` copies the file descriptor table. The child gets a working
//! descriptor for every door the parent had created. That is bad in two
//! ways:
//!
//! - The kernel counts references to a door. While the child holds a
//!   copy, the parent's `DOOR_UNREF` notification does not fire. So a
//!   child that never wanted the door changes the parent's behaviour.
//! - The child owns a `Door` value that looks alive. If the child drops
//!   it, `Door::drop` would `door_revoke`, `fdetach` and `unlink` the
//!   parent's door path. The parent's door would disappear because an
//!   unrelated child exited.
//!
//! # The answer
//!
//! Every live server door joins a process-global registry. On `fork`,
//! a `pthread_atfork` child handler walks the registry, marks each door
//! *disowned*, and closes its descriptor. After that the child's `Door`
//! values are inert: they still exist, but every operation on them
//! refuses, and dropping one tears down nothing.
//!
//! A `Door` does **not** survive `fork`. A child that wants a door must
//! create its own.
//!
//! # The limitation to know about
//!
//! There is no way to unregister a `pthread_atfork` handler. If this
//! crate is linked into a `cdylib` that is later `dlclose`d, the
//! handler addresses become invalid and a later `fork` will jump into
//! freed memory. That is a property of `pthread_atfork(3C)`, not
//! something this module can fix, so there is deliberately no cargo
//! feature to turn the handler off.

use crate::sys;
use doors_sys::Errno;
use std::cell::UnsafeCell;
use std::os::fd::RawFd;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Once};

/// The part of a server door that the fork machinery needs to reach.
///
/// `Door<S>` is generic over the user's state, and the registry cannot
/// be generic: it holds every door in the process at once. So the few
/// fields the atfork handlers touch live here, in a type that knows
/// nothing about `S`.
///
/// This value never moves. `Door<S>` holds an `Arc<DoorInner>` and so
/// does the registry, which means the handlers can reach these fields
/// through a stable address no matter where the `Door` itself was
/// moved to.
#[derive(Debug)]
pub(crate) struct DoorInner {
    /// The door descriptor, or `-1` once it has been released.
    ///
    /// Atomic, and swapped rather than merely stored, because two
    /// places release it: [`deregister_and_release`] on the normal
    /// path, and the atfork child handler after a `fork`. Whichever
    /// gets there first leaves `-1` behind, so the other one releases
    /// nothing. Releasing twice would be worse than a leak: the number
    /// may already name a different file by then.
    pub(crate) fd: AtomicI32,

    /// Set once a `fork` handed this door to a child process.
    ///
    /// A disowned door refuses `revoke`, `detach` and teardown on
    /// `Drop`. It is one-way: nothing ever clears it.
    pub(crate) disowned: AtomicBool,

    /// The pid of the process that created the door.
    ///
    /// A second, independent check. `vfork` and `forkall` do not run
    /// atfork handlers the way `fork` does, so `disowned` alone can be
    /// missed. Comparing pids catches those cases without needing any
    /// handler to have run.
    pub(crate) owner_pid: libc::pid_t,

    /// How many calls are inside the server procedure right now.
    ///
    /// `Door::revoke()` waits for this to reach zero before it hands
    /// the user's state back, so state cannot be dropped underneath a
    /// call that is still running.
    pub(crate) in_flight: AtomicUsize,
}

impl DoorInner {
    /// Wrap a fresh door descriptor.
    ///
    /// Returns an `Arc` because there are always at least two owners:
    /// the `Door` and the registry.
    pub(crate) fn new(fd: RawFd) -> Arc<Self> {
        // SAFETY: getpid always succeeds and touches no memory.
        let owner_pid = unsafe { libc::getpid() };
        Arc::new(DoorInner {
            fd: AtomicI32::new(fd),
            disowned: AtomicBool::new(false),
            owner_pid,
            in_flight: AtomicUsize::new(0),
        })
    }

    /// Did a `fork` take this door away from us?
    ///
    /// `Acquire` pairs with the `Release` store in the child handler.
    /// That publishes the flag and everything the handler did before
    /// setting it. It says nothing about `fd`, which the handler swaps
    /// *after* the store, so seeing `true` here does not mean `fd` is
    /// already `-1`. Anything that needs the descriptor must read it
    /// and treat `-1` as gone, as [`raw_fd`](Self::raw_fd) says.
    pub(crate) fn is_disowned(&self) -> bool {
        self.disowned.load(Ordering::Acquire)
    }

    /// Are we still the process that made this door?
    pub(crate) fn is_owner(&self) -> bool {
        // SAFETY: getpid always succeeds and touches no memory.
        let now = unsafe { libc::getpid() };
        now == self.owner_pid
    }

    /// The descriptor, or `-1` if it has been closed.
    ///
    /// Callers must treat `-1` as "gone" rather than passing it to a
    /// door call.
    pub(crate) fn raw_fd(&self) -> RawFd {
        self.fd.load(Ordering::Acquire)
    }
}

// --------------------------------------------------------------------
// The lock
// --------------------------------------------------------------------

/// A raw POSIX mutex living in a `static`.
///
/// # Why not `std::sync::Mutex`
///
/// This lock has to be **taken by one function and released by a
/// different one**, in a different process. `pthread_atfork` gives us
/// three separate C callbacks: `prepare` locks, then either `parent`
/// or `child` unlocks. A `std::sync::Mutex` can only be unlocked by
/// dropping its guard, and there is nowhere to keep a guard alive
/// across `fork` and hand it to whichever callback runs next. Rust's
/// mutex also has no defined behaviour when it is inherited by a
/// forked child.
///
/// A `pthread_mutex_t` has exactly the shape we need: `lock` and
/// `unlock` are two independent calls on a static address, and the
/// address is the same in parent and child because `fork` copies the
/// address space. This is the idiom `pthread_atfork(3C)` was designed
/// for.
///
/// Strictly, POSIX only defines unlocking by the owning thread. The
/// child's single thread is the copy of the thread that called `fork`
/// — the same thread that ran `prepare` — so from libc's point of view
/// the owner is the one unlocking.
struct RawMutex(UnsafeCell<libc::pthread_mutex_t>);

// SAFETY: a pthread mutex is designed to be shared by every thread in
// the process. All access goes through pthread_mutex_lock/unlock,
// which do their own synchronisation.
unsafe impl Sync for RawMutex {}

/// The one lock that guards the registry.
static REGISTRY_LOCK: RawMutex =
    RawMutex(UnsafeCell::new(libc::PTHREAD_MUTEX_INITIALIZER));

/// Take the registry lock.
///
/// # Safety
///
/// The caller must release it with [`unlock_registry`], and must not
/// take it twice on one thread: the mutex is not recursive.
unsafe fn lock_registry() {
    // The return value is ignored on purpose. A non-recursive mutex
    // can only fail here through misuse, and this runs inside atfork
    // handlers where panicking or formatting an error is not allowed.
    let _ = libc::pthread_mutex_lock(REGISTRY_LOCK.0.get());
}

/// Release the registry lock.
///
/// # Safety
///
/// The lock must currently be held by this thread, or — after a `fork`
/// — by the thread this one was copied from.
unsafe fn unlock_registry() {
    let _ = libc::pthread_mutex_unlock(REGISTRY_LOCK.0.get());
}

// --------------------------------------------------------------------
// The storage
// --------------------------------------------------------------------

/// How many doors fit in one chunk.
///
/// Most programs create a handful of doors, so one chunk is usually
/// the whole registry.
const CHUNK_SLOTS: usize = 64;

/// A fixed block of registry slots.
///
/// # Why chunks and not a `Vec`
///
/// The atfork child handler walks this structure and may not allocate
/// (see [`atfork_child`]). A `Vec` moves its elements to a new
/// allocation when it grows, and a growing `Vec` frees the old buffer.
/// Chunks never move and are never freed, so a pointer into one stays
/// valid for the life of the process. Growing means linking one more
/// chunk on the end, which leaves every existing chunk exactly where
/// it was.
///
/// Not freeing them is deliberate. The memory is bounded by the
/// largest number of doors ever alive at once, and freeing would mean
/// proving that no handler is part-way through a walk — which is
/// exactly the kind of proof that is impossible across `fork`.
struct Chunk {
    /// `None` means the slot is free.
    slots: [Option<Arc<DoorInner>>; CHUNK_SLOTS],
    /// The next chunk, or null. Written once, under the lock.
    next: *mut Chunk,
}

/// The head of the chunk list.
struct RegistryHead(UnsafeCell<*mut Chunk>);

// SAFETY: every read and write of this pointer happens with
// REGISTRY_LOCK held — including in the atfork child handler, which
// inherits the lock already taken by `prepare`.
unsafe impl Sync for RegistryHead {}

/// Every server door in this process.
static HEAD: RegistryHead = RegistryHead(UnsafeCell::new(ptr::null_mut()));

/// Make an empty chunk that will live until the process exits.
fn new_chunk() -> *mut Chunk {
    Box::into_raw(Box::new(Chunk {
        slots: std::array::from_fn(|_| None),
        next: ptr::null_mut(),
    }))
}

/// Put `inner` in the first free slot and return its key.
///
/// Returns `None` when every existing chunk is full. It does not grow
/// the registry itself, because growing allocates and this runs with
/// the lock held; see [`register`].
///
/// # Safety
///
/// The registry lock must be held.
unsafe fn try_insert_locked(inner: &Arc<DoorInner>) -> Option<usize> {
    let mut chunk = *HEAD.0.get();
    let mut base = 0usize;
    while !chunk.is_null() {
        for i in 0..CHUNK_SLOTS {
            let slot = (*chunk).slots.as_mut_ptr().add(i);
            if (*slot).is_none() {
                *slot = Some(Arc::clone(inner));
                return Some(base + i);
            }
        }
        base += CHUNK_SLOTS;
        chunk = (*chunk).next;
    }
    None
}

/// Link an already-built chunk onto the end of the list.
///
/// # Safety
///
/// The registry lock must be held, and `chunk` must come from
/// [`new_chunk`] and not be linked anywhere else.
unsafe fn link_chunk_locked(chunk: *mut Chunk) {
    let mut cursor = HEAD.0.get();
    while !(*cursor).is_null() {
        cursor = ptr::addr_of_mut!((**cursor).next);
    }
    *cursor = chunk;
}

/// Find the slot a key names, or null if the registry never grew that
/// far.
///
/// # Safety
///
/// The registry lock must be held, and the returned pointer may only
/// be used while it stays held.
unsafe fn slot_ptr(key: usize) -> *mut Option<Arc<DoorInner>> {
    let mut chunk = *HEAD.0.get();
    let mut skip = key / CHUNK_SLOTS;
    while !chunk.is_null() && skip > 0 {
        chunk = (*chunk).next;
        skip -= 1;
    }
    if chunk.is_null() {
        return ptr::null_mut();
    }
    (*chunk).slots.as_mut_ptr().add(key % CHUNK_SLOTS)
}

// --------------------------------------------------------------------
// Joining and leaving
// --------------------------------------------------------------------

/// Add a door to the registry and return the key that names it.
///
/// The caller keeps the key and hands it back to
/// [`deregister_and_release`], along with the same `Arc` it registered.
/// A key names a *slot*, and a freed slot is handed straight back out
/// to the next door, so the key alone is not enough to identify who
/// the entry belongs to.
///
/// The lock is taken and released twice in the worst case, on purpose.
/// Growing the registry allocates, and allocating while holding this
/// lock is a deadlock risk: a `fork` on another thread runs handlers
/// that take libc's own locks, so a chain of "we hold the registry
/// lock and want the allocator lock" against "we hold the allocator
/// lock and want the registry lock" is possible. Building the chunk
/// outside the lock removes that chain entirely. Two threads racing may
/// each build a chunk; the loser's chunk is simply spare capacity.
pub(crate) fn register(inner: Arc<DoorInner>) -> usize {
    loop {
        // SAFETY: locked and unlocked as a pair, with nothing between
        // that can take the lock again.
        unsafe {
            lock_registry();
            let placed = try_insert_locked(&inner);
            unlock_registry();
            if let Some(key) = placed {
                return key;
            }
        }

        // Full. Build capacity without holding anything, then retry.
        let chunk = new_chunk();
        // SAFETY: chunk is freshly built and linked exactly once.
        unsafe {
            lock_registry();
            link_chunk_locked(chunk);
            unlock_registry();
        }
    }
}

/// Remove a door from the registry and release its descriptor.
///
/// Returns `Some(errno)` only when `revoke` was asked for and
/// `door_revoke` failed. `None` means the descriptor is gone, or that
/// this call found nothing of ours left to remove.
///
/// # There are two ways to release a door descriptor
///
/// `revoke` picks between them, and there is no third choice:
///
/// - `true` — the door belongs to this process, so `door_revoke(3C)`
///   is the release. **`door_revoke` closes the descriptor itself.**
///   It does not merely invalidate the door. So there is no `close`
///   after it, and adding one back would be a double close. We have
///   seen that go wrong: the second close shut a descriptor number
///   another thread had just been given.
/// - `false` — a `fork` handed us a copy of a door the parent still
///   serves. Revoking would kill the parent's door, so a plain
///   `close(2)` is the release here. It drops our copy only.
///
/// # The order here is the point of this module
///
/// The lock is taken, the entry is removed, the descriptor is
/// released, and only then is the lock released. All four steps, in
/// that order.
///
/// Releasing first and deregistering afterwards would be a real bug,
/// not a style question. Suppose this thread frees fd 7 and then stops
/// before removing the entry. Another thread opens a file and the
/// kernel hands back fd 7, because it was just freed. Now a `fork`
/// happens: the child handler walks the registry, finds the entry that
/// still says 7, and closes it — closing someone else's brand new
/// file. Holding the lock across both steps makes that impossible,
/// because the handler cannot start its walk until the entry and the
/// descriptor agree again.
///
/// Deregistration happens even when the door is disowned. A disowned
/// door skips `door_revoke`, `fdetach` and `unlink` — those would
/// damage the parent — but its registry entry is this process's own
/// bookkeeping and must still go.
///
/// Calling this twice for the same door is harmless. The entry is
/// matched by identity and not by key alone, so the second call does
/// nothing even when the slot has since been handed to a different
/// door — and that case is not rare. [`try_insert_locked`] fills the
/// first free slot, so the very next door built after this one is
/// removed will usually take the slot back. Trusting the key on its
/// own would release a descriptor another live `Door` still believes
/// it owns, which is the fd-reuse bug the rest of this function exists
/// to prevent.
///
/// # When `door_revoke` fails
///
/// The descriptor is left alone. We do not fall back to `close`. If
/// the failure was `EBADF` the number is already free, and another
/// thread may hold it by now, so closing it would be the very bug this
/// function is written to stop. Leaking one descriptor on a path that
/// should never be taken is the cheaper mistake.
pub(crate) fn deregister_and_release(
    inner: &Arc<DoorInner>,
    key: usize,
    revoke: bool,
) -> Option<Errno> {
    // SAFETY: the lock is held across the whole critical section, and
    // slot_ptr's result is used only inside it. The calls made while
    // holding it — door_revoke, close and reading errno — take no lock
    // of their own and never allocate, so they cannot deadlock against
    // a concurrent fork.
    let (removed, failure) = unsafe {
        lock_registry();

        let slot = slot_ptr(key);
        let mine = !slot.is_null()
            && matches!(&*slot, Some(cur) if Arc::ptr_eq(cur, inner));
        let taken = if mine { (*slot).take() } else { None };

        let mut failure = None;
        if let Some(inner) = taken.as_ref() {
            // Swap to -1 in the same step that reads the number, so a
            // fork that happens after we release the lock finds
            // nothing left to release, and so the atfork child handler
            // and this function can never both act on one descriptor.
            let fd = inner.fd.swap(-1, Ordering::AcqRel);
            if fd >= 0 {
                if revoke {
                    // door_revoke CLOSES fd. Never add a close after
                    // this call: that is the double close of
                    // `docs/DESIGN.md` Appendix E.
                    if sys::door_revoke(fd) < 0 {
                        // Read errno here, before unlocking, because
                        // pthread_mutex_unlock is free to change it.
                        failure = Some(sys::last_errno());
                    }
                } else {
                    libc::close(fd);
                }
            }
        }

        unlock_registry();
        (taken, failure)
    };

    // Dropping the Arc may free. That is done outside the lock, so the
    // allocator is never entered while the registry is held.
    drop(removed);

    failure
}

// --------------------------------------------------------------------
// The atfork handlers
// --------------------------------------------------------------------

/// Runs in the parent, just before `fork`, while every other thread is
/// still running.
///
/// It takes the registry lock. That is what makes the child handler
/// legal: the child inherits the lock already held, and inherits a
/// registry that no half-finished update can be sitting in the middle
/// of. So the child never has to acquire anything.
extern "C" fn atfork_prepare() {
    // SAFETY: released by atfork_parent or atfork_child, exactly one
    // of which always runs.
    unsafe { lock_registry() }
}

/// Runs in the parent after `fork`. The parent keeps all its doors, so
/// there is nothing to do but let go of the lock.
extern "C" fn atfork_parent() {
    // SAFETY: taken by atfork_prepare on this thread.
    unsafe { unlock_registry() }
}

/// Runs in the child after `fork`. Disowns and closes every server
/// door.
///
/// # What this function may and may not do
///
/// Between `fork` and `exec` the child has one thread, but it inherited
/// the *locks* of every thread the parent had. Any lock a parent thread
/// was holding at the moment of the fork is now held by a thread that
/// does not exist, so it will never be released. Touching anything that
/// takes such a lock hangs the child forever. That is why POSIX limits
/// this code to async-signal-safe work.
///
/// Allowed, and all this function does:
///
/// - atomic loads, stores and swaps — no lock involved,
/// - `close(2)` — an async-signal-safe system call,
/// - reading pointers and slots that `prepare` froze for us.
///
/// Not allowed, and deliberately absent:
///
/// - **allocating or freeing**, which is why the storage is chunked and
///   why no `Arc` is dropped here. Dropping the last `Arc` to a
///   `DoorInner` would call `free`,
/// - `println!`, formatting, or any logging,
/// - taking any Rust `Mutex`, or `pthread_mutex_lock` on the registry —
///   `prepare` already holds it for us,
/// - anything that can panic, since unwinding out of an `extern "C"`
///   function aborts the process.
///
/// The walk borrows each entry with `as_ref` and never removes it. The
/// slots keep their `Arc`s; only the flag and the descriptor change.
extern "C" fn atfork_child() {
    // SAFETY: the child is single-threaded and atfork_prepare left the
    // registry locked and consistent, so nothing can be modifying the
    // chunk list while we walk it. Chunks are never moved or freed, so
    // every pointer we follow is still valid.
    unsafe {
        let mut chunk = *HEAD.0.get();
        while !chunk.is_null() {
            for slot in (*chunk).slots.iter() {
                if let Some(inner) = slot.as_ref() {
                    // Disown first, and never the other way round. A
                    // thread in the child that reads the flag while it
                    // is still clear goes on to revoke, fdetach and
                    // unlink. Closing the fd first would open a window
                    // where the descriptor is already gone but the
                    // door still looks owned, and that thread would
                    // tear down the parent's door path
                    // (`GOALS.md` §12.8). The opposite window —
                    // disowned set, fd still live — is harmless: the
                    // swap below is atomic, so only one closer wins.
                    inner.disowned.store(true, Ordering::Release);

                    let fd = inner.fd.swap(-1, Ordering::AcqRel);
                    if fd >= 0 {
                        // close, and never door_revoke. This is the
                        // parent's door; revoking would destroy it for
                        // the parent too. Closing only drops the copy
                        // the fork gave us, and that is what keeps the
                        // parent's DOOR_UNREF timing correct: while
                        // the child held a copy, the door still had a
                        // reference.
                        libc::close(fd);
                    }
                }
            }
            chunk = (*chunk).next;
        }

        unlock_registry();
    }
}

/// Guards the one-time `pthread_atfork` registration.
static ATFORK: Once = Once::new();

/// Register the atfork handlers, once per process.
///
/// Lazy on purpose. A program that never creates a server door pays
/// nothing, and `pthread_atfork` handlers can never be removed, so we
/// only want them installed when there is something to protect.
///
/// Safe to call from anywhere and as often as you like. It must be
/// called before the first `fork` that could see a door, so
/// `DoorBuilder::build` calls it when a door is created and
/// [`fork`] calls it again as a second chance.
///
/// Note it must not be called with the registry lock held:
/// `pthread_atfork` may allocate.
pub(crate) fn install_atfork_once() {
    ATFORK.call_once(|| {
        // SAFETY: the three handlers are ordinary extern "C" functions
        // with static lifetime, and the pointers stay valid as long as
        // this code is mapped.
        let rc = unsafe {
            sys::pthread_atfork(
                Some(atfork_prepare),
                Some(atfork_parent),
                Some(atfork_child),
            )
        };
        // Nothing useful to do on failure, and no error type to put it
        // in: the caller is building a door, not forking. A failed
        // registration means the backstop is missing, but layers one
        // and two of GOALS.md §7 — FD_CLOEXEC and doors::fork — still
        // apply.
        let _ = rc;
    });
}

// --------------------------------------------------------------------
// fork
// --------------------------------------------------------------------

/// Which side of a [`fork`] you are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkResult {
    /// The original process. Carries the child's pid, for `waitpid`.
    Parent {
        /// The new child's process id.
        child: libc::pid_t,
    },
    /// The new process.
    Child,
}

/// Fork the process, with this crate's doors handled correctly.
///
/// # What it does for you
///
/// By the time this returns in the child, every server door in the
/// process has already been disowned and closed. The atfork child
/// handler did it, in between `fork` returning in the kernel and
/// control arriving back here. So this function itself does no
/// cleanup — there is nothing left to clean up.
///
/// The child's `Door` values still exist as Rust values. They are
/// inert: revoking, detaching and dropping one all do nothing to the
/// parent's door or its path.
///
/// # What it deliberately does not do
///
/// [`Client`](crate::Client) handles are left completely alone. A child
/// may keep calling a door it inherited, and often that is exactly the
/// point: the parent opens the door, forks, and both processes talk to
/// the server. Closing them would break that for no benefit, since a
/// client descriptor is just a reference to someone else's door.
///
/// # The usual `fork` warning still applies
///
/// This is `fork`, not `posix_spawn`. In a program with more than one
/// thread, the child inherits locks held by threads that no longer
/// exist. Allocating, logging, or almost anything in the standard
/// library can hang the child. The safe shapes are: call `exec` soon,
/// or `_exit`. That hazard belongs to `fork` itself; this wrapper only
/// promises that *doors* are in a sane state.
///
/// ```no_run
/// # fn main() -> std::io::Result<()> {
/// use doors::{fork, ForkResult};
///
/// match fork()? {
///     ForkResult::Parent { child } => {
///         println!("child is {child}");
///     }
///     ForkResult::Child => {
///         // The parent's server doors are already closed here.
///         std::process::exit(0);
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub fn fork() -> std::io::Result<ForkResult> {
    // Installed here too, in case the caller forks before ever having
    // built a door. Cheap after the first call.
    install_atfork_once();

    // SAFETY: fork takes no arguments and touches no memory of ours.
    // The handlers registered above run inside this call.
    let pid = unsafe { libc::fork() };

    match pid {
        -1 => Err(std::io::Error::last_os_error()),
        0 => Ok(ForkResult::Child),
        child => Ok(ForkResult::Parent { child }),
    }
}

// --------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------

/// Look a door up by key. Test-only: nothing in the crate needs it,
/// but a test cannot check the registry without reading it.
#[cfg(test)]
fn lookup(key: usize) -> Option<Arc<DoorInner>> {
    // SAFETY: the lock is held for the whole read, and the clone
    // happens before it is released.
    unsafe {
        lock_registry();
        let slot = slot_ptr(key);
        let found = if slot.is_null() {
            None
        } else {
            (*slot).clone()
        };
        unlock_registry();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::IntoRawFd;

    /// A real descriptor to register, so that closing it is a normal
    /// close and not a guess at some other test's fd.
    fn spare_fd() -> RawFd {
        std::fs::File::open("/dev/null")
            .expect("/dev/null opens")
            .into_raw_fd()
    }

    #[test]
    fn register_then_deregister_round_trip() {
        let inner = DoorInner::new(spare_fd());
        let key = register(Arc::clone(&inner));

        let found = lookup(key).expect("a registered door is findable");
        assert!(Arc::ptr_eq(&found, &inner));
        drop(found);

        deregister_and_release(&inner, key, false);

        // The slot itself may already be full again: a freed slot goes
        // straight to the next door, and another thread may have taken
        // it before this line runs. What must be true is that the slot
        // no longer holds *this* door.
        assert!(!holds(key, &inner), "our door left the registry");
        assert_eq!(inner.raw_fd(), -1, "the descriptor was closed");
    }

    #[test]
    fn deregister_twice_is_harmless() {
        let inner = DoorInner::new(spare_fd());
        let key = register(Arc::clone(&inner));
        deregister_and_release(&inner, key, false);
        deregister_and_release(&inner, key, false);
        // Same reason as above: check identity, not emptiness.
        assert!(!holds(key, &inner));
    }

    /// Does slot `key` still hold this exact door?
    ///
    /// Tests must ask this rather than "is the slot empty", because a
    /// free slot is handed to the next door straight away and another
    /// thread's door may already be sitting there.
    fn holds(key: usize, inner: &Arc<DoorInner>) -> bool {
        matches!(lookup(key), Some(cur) if Arc::ptr_eq(&cur, inner))
    }

    #[test]
    fn a_stale_key_cannot_close_the_door_that_took_the_slot() {
        let first = DoorInner::new(spare_fd());
        let key = register(Arc::clone(&first));
        deregister_and_release(&first, key, false);

        // The freed slot is the first free one, so the next door in
        // usually lands right back on it.
        let second = DoorInner::new(spare_fd());
        let second_key = register(Arc::clone(&second));

        // A repeat of the first door's teardown must not touch it.
        deregister_and_release(&first, key, false);
        if second_key == key {
            assert!(lookup(key).is_some(), "the new door is still listed");
        }
        assert!(second.raw_fd() >= 0, "the new door's fd is still open");

        deregister_and_release(&second, second_key, false);
        assert_eq!(second.raw_fd(), -1);
    }

    #[test]
    fn keys_stay_distinct_past_one_chunk() {
        let count = CHUNK_SLOTS * 2 + 3;
        let mut keys = Vec::with_capacity(count);
        let mut doors = Vec::with_capacity(count);

        for _ in 0..count {
            let inner = DoorInner::new(spare_fd());
            keys.push(register(Arc::clone(&inner)));
            doors.push(inner);
        }

        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), count, "every key is its own slot");

        for (key, inner) in keys.iter().zip(doors.iter()) {
            let found = lookup(*key).expect("still registered");
            assert!(Arc::ptr_eq(&found, inner));
        }

        for (key, inner) in keys.iter().zip(doors.iter()) {
            deregister_and_release(inner, *key, false);
        }
        for inner in doors {
            assert_eq!(inner.raw_fd(), -1);
        }
    }

    #[test]
    fn owner_is_the_creating_process() {
        let inner = DoorInner::new(-1);
        assert!(inner.is_owner(), "we just made it");

        // A door made by some other process, faked by hand.
        let alien = DoorInner {
            fd: AtomicI32::new(-1),
            disowned: AtomicBool::new(false),
            owner_pid: inner.owner_pid.wrapping_add(1),
            in_flight: AtomicUsize::new(0),
        };
        assert!(!alien.is_owner());
    }

    #[test]
    fn disowned_starts_clear_and_only_ever_sets() {
        let inner = DoorInner::new(-1);
        assert!(!inner.is_disowned());

        // What the atfork child handler does.
        inner.disowned.store(true, Ordering::Release);
        assert!(inner.is_disowned());
    }

    #[test]
    fn closing_leaves_minus_one_behind() {
        let inner = DoorInner::new(spare_fd());
        let key = register(Arc::clone(&inner));
        assert!(inner.raw_fd() >= 0);
        deregister_and_release(&inner, key, false);
        // The second closer must see -1 and do nothing, or it would
        // close a descriptor number that has since been reused.
        assert_eq!(inner.raw_fd(), -1);
    }

    /// Many threads registering and deregistering at the same time.
    ///
    /// This is the test that catches the fd-reuse bug. Two live doors
    /// must never hold the same key. If they ever do, one door's
    /// teardown closes the other door's descriptor, and the other door
    /// then fails every call with `EBADF`.
    ///
    /// Two checks run together:
    ///
    /// - `live` holds every key that belongs to a door that is still
    ///   registered. A key that is already in there when a new door
    ///   claims it means the registry handed the same slot to two
    ///   doors at once.
    /// - Looking the key straight back up must find *my* door. If it
    ///   finds someone else's, my entry was overwritten.
    ///
    /// A key is removed from `live` *before* the door leaves the
    /// registry, never after. That order matters: the slot cannot be
    /// handed to anyone else until the door leaves, so no honest
    /// hand-out can ever look like a duplicate.
    #[test]
    fn concurrent_register_never_hands_out_a_live_key_twice() {
        use std::collections::HashSet;
        use std::sync::Mutex;

        const THREADS: usize = 8;
        const ROUNDS: usize = 400;
        /// Enough live doors at once to need more than one chunk.
        const HELD: usize = 12;

        let live: Arc<Mutex<HashSet<usize>>> = Arc::default();
        let start = Arc::new(std::sync::Barrier::new(THREADS));

        let mut threads = Vec::with_capacity(THREADS);
        for _ in 0..THREADS {
            let live = Arc::clone(&live);
            let start = Arc::clone(&start);
            threads.push(std::thread::spawn(move || {
                start.wait();
                let mut held: Vec<(Arc<DoorInner>, usize)> = Vec::new();

                for _ in 0..ROUNDS {
                    let inner = DoorInner::new(spare_fd());
                    let key = register(Arc::clone(&inner));

                    {
                        let mut live = live.lock().unwrap();
                        assert!(
                            live.insert(key),
                            "key {key} was handed to two live doors"
                        );
                    }

                    let found = lookup(key).expect("my door is registered");
                    assert!(
                        Arc::ptr_eq(&found, &inner),
                        "slot {key} holds somebody else's door"
                    );
                    drop(found);

                    assert!(
                        inner.raw_fd() >= 0,
                        "somebody closed my descriptor early"
                    );

                    held.push((inner, key));
                    if held.len() > HELD {
                        let (inner, key) = held.remove(0);
                        live.lock().unwrap().remove(&key);
                        deregister_and_release(&inner, key, false);
                        assert_eq!(inner.raw_fd(), -1);
                    }
                }

                for (inner, key) in held {
                    live.lock().unwrap().remove(&key);
                    deregister_and_release(&inner, key, false);
                }
            }));
        }

        for t in threads {
            t.join().expect("no thread panicked");
        }
        assert!(live.lock().unwrap().is_empty(), "every door left");
    }
}
