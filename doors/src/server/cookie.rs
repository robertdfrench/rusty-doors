// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Turning a door cookie back into server state.
//!
//! # The problem
//!
//! The kernel keeps one machine word for each door, and hands it to
//! the server procedure on every call. That word is the *cookie*. The
//! kernel never looks inside it. It is a number, nothing more.
//!
//! C door servers put a pointer in it and dereference it. That works
//! until the door is revoked and the state is freed. A call already on
//! its way in then reads freed memory, and the server crashes or worse.
//!
//! **Never dereference a cookie without going through the lookup.**
//! Every path from a cookie to server state in this crate goes through
//! [`resolve`], and resolution is allowed to fail. A failure becomes a
//! `GOALS.md` §3.9 tag `2` reply carrying
//! [`ServerFault::StateUnavailable`], and touches no memory at all.
//!
//! # How the cookie works
//!
//! The cookie is a *slot number* in a table, not a pointer. It carries
//! a generation counter alongside the slot number, so a number that
//! has been retired stays retired.
//!
//! Resolving takes the table's lock, checks the generation, and clones
//! the `Arc<T>` out — all in one step, under the one lock. That is
//! what makes it sound. "Is the state still there?" and "take a
//! reference to it" cannot come apart, so a call already running keeps
//! its own reference and the state outlives the call even if another
//! thread revokes the door in the middle of it. A stale cookie simply
//! gives `None`. There is nothing to dereference, so a cookie that
//! outlives its door cannot hurt anyone.
//!
//! # Why there is no pointer fast path
//!
//! There used to be a second choice, `Pinned`, where the cookie was a
//! leaked `Arc<T>` pointer. Resolving was a pointer read and a
//! reference count bump: no table, no lock, faster.
//!
//! It could not be made sound, so it is gone. Resolving means turning
//! a raw pointer back into a `+1` reference count, and that is only
//! valid while the allocation is still alive. A raw pointer cannot
//! tell you whether it is. This is exactly why `Arc` offers no such
//! operation. The table below gets away with it only because its lock
//! makes "check alive" and "take a reference" a single atomic step.
//!
//! `cmd/svc/configd/client.c:2303` in illumos uses this same design —
//! an integer handle, a locked lookup, and a reference count. It is
//! the best door consumer in the gate, and this is the shape it chose.
//!
//! # Why the slab is type-erased
//!
//! `GOALS.md` §5.3 asks for "a process-global slab". A plain `static`
//! cannot be generic, so there is no way to write one slab per `T`.
//! The slab here is genuinely process-global instead, and stores
//! `Arc<dyn Any + Send + Sync>`. Resolution downcasts back to `T`.
//!
//! That buys two things beyond the wording:
//!
//! - One table for the whole process, so a door costs one slot, not
//!   one allocation plus a leak.
//! - A cookie for the wrong type resolves to `None` rather than
//!   reinterpreting somebody else's state.
//!
//! It costs a `T: Send + Sync + 'static` bound. Door state is shared
//! by every server thread and outlives the call, so that bound is
//! needed anyway.
//!
//! [`ServerFault::StateUnavailable`]: crate::error::ServerFault

use crate::error::ServerFault;
use std::any::Any;
use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

// --------------------------------------------------------------------
// The process-global slab
// --------------------------------------------------------------------

/// How many bits of the cookie word hold the slot number. The other
/// half holds the generation.
const INDEX_BITS: u32 = usize::BITS / 2;

/// The widest slot number, and the widest generation.
const INDEX_MASK: usize = (1usize << INDEX_BITS) - 1;

/// One place in the slab.
///
/// `generation` counts how many times this slot has been handed out.
/// It is the whole reason a stale cookie is safe: slot numbers get
/// reused, generations do not, so an old cookie fails to match the
/// slot it used to own instead of quietly reading the new tenant.
///
/// "Generations do not get reused" only holds because a slot whose
/// generation would wrap is retired instead of freed; see
/// [`slab_uninstall`]. A generation of zero marks such a slot, and no
/// cookie ever carries a zero generation, so a retired slot matches
/// nothing for the rest of the process's life.
struct Slot {
    generation: usize,
    state: Option<Arc<dyn Any + Send + Sync>>,
}

/// Every door in the process.
///
/// The state is stored type-erased; see the module docs for why.
struct Slab {
    slots: Vec<Slot>,
    /// Slot numbers free for reuse, so a process that builds and
    /// revokes many doors does not grow the table forever.
    free: Vec<usize>,
}

impl Slab {
    const fn new() -> Self {
        Slab {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

/// An `RwLock`, not a `Mutex`, because resolving happens on every call
/// on every server thread and only ever reads. Installing and
/// uninstalling happen once per door, so they can afford to wait.
static SLAB: RwLock<Slab> = RwLock::new(Slab::new());

/// Take the read lock, ignoring poison.
///
/// A panic somewhere else must not stop every door in the process
/// from answering. The slab holds `Arc`s and indices; a panic cannot
/// leave it half-written in a way that matters, because every write
/// below finishes before it releases the lock.
fn read_slab() -> RwLockReadGuard<'static, Slab> {
    SLAB.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Take the write lock, ignoring poison. Same reasoning as
/// [`read_slab`].
fn write_slab() -> RwLockWriteGuard<'static, Slab> {
    SLAB.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Build the cookie word from a slot number and a generation.
fn pack(index: usize, generation: usize) -> usize {
    ((generation & INDEX_MASK) << INDEX_BITS) | (index & INDEX_MASK)
}

/// Split a cookie word back apart.
fn unpack(word: usize) -> (usize, usize) {
    (word & INDEX_MASK, (word >> INDEX_BITS) & INDEX_MASK)
}

/// Step a generation, or `None` once there is no next one.
///
/// Generations start at one and never reach zero, so a packed cookie
/// always has a non-zero upper half. That means a cookie is never a
/// null pointer, and a null cookie is always a bug we can reject on
/// sight.
///
/// The generation is only half a word — sixteen bits on a 32-bit
/// target — so it can run out. Starting over at one would make the
/// oldest cookies for that slot match again, which is exactly the
/// stale-cookie read this module exists to prevent. So there is no
/// next generation: the caller retires the slot instead.
fn next_generation(current: usize) -> Option<usize> {
    match (current + 1) & INDEX_MASK {
        0 => None,
        n => Some(n),
    }
}

/// Put state in a free slot and return the cookie word.
///
/// # Panics
///
/// If the process has more live doors than a slot number can count —
/// over four billion on a 64-bit machine. That is an out-of-resources
/// condition, like a failed allocation.
fn slab_install(state: Arc<dyn Any + Send + Sync>) -> usize {
    let mut slab = write_slab();

    let index = match slab.free.pop() {
        Some(i) => i,
        None => {
            let i = slab.slots.len();
            assert!(i <= INDEX_MASK, "too many live doors");
            slab.slots.push(Slot {
                generation: 1,
                state: None,
            });
            i
        }
    };

    let slot = &mut slab.slots[index];
    slot.state = Some(state);
    pack(index, slot.generation)
}

/// Look a cookie word up. Any word is allowed; a wrong one gives
/// `None`.
fn slab_resolve(word: usize) -> Option<Arc<dyn Any + Send + Sync>> {
    if word == 0 {
        return None;
    }
    let (index, generation) = unpack(word);

    let slab = read_slab();
    let slot = slab.slots.get(index)?;
    if slot.generation != generation {
        return None;
    }
    slot.state.clone()
}

/// Empty a slot, so every later resolve of that cookie says `None`.
fn slab_uninstall(word: usize) -> Option<Arc<dyn Any + Send + Sync>> {
    let (index, generation) = unpack(word);

    let mut slab = write_slab();
    let slot = slab.slots.get_mut(index)?;
    if slot.generation != generation {
        return None;
    }
    let taken = slot.state.take()?;

    // Move the slot on before anyone can reuse it, so the cookie we
    // just retired cannot match again.
    //
    // When the generation runs out there is no such number left, so
    // the slot is retired rather than freed: generation zero, and
    // never on the free list. It costs one slot forever and buys back
    // the guarantee this whole module rests on.
    match next_generation(slot.generation) {
        Some(next) => {
            slot.generation = next;
            slab.free.push(index);
        }
        None => slot.generation = 0,
    }

    // Release the lock before the caller can drop the last reference:
    // running the state's destructor under the slab lock would let a
    // destructor that touches doors deadlock.
    drop(slab);
    Some(taken)
}

// --------------------------------------------------------------------
// The cookie, as the rest of the crate sees it
// --------------------------------------------------------------------

/// Proof that a slot in the slab is still ours.
///
/// Holds the cookie word, which is enough to find the slot again and
/// to check that nobody else has taken it over meanwhile.
///
/// It is not `Copy` and not `Clone` on purpose. Exactly one ticket
/// exists per installed door, so the state cannot be taken out twice.
pub struct Ticket<T> {
    word: usize,
    _state: PhantomData<fn() -> T>,
}

// Written out rather than derived: a derive would demand `T: Debug`,
// and the ticket does not print the state, only the slot it is in.
impl<T> fmt::Debug for Ticket<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (index, generation) = unpack(self.word);
        f.debug_struct("Ticket")
            .field("slot", &index)
            .field("generation", &generation)
            .finish()
    }
}

/// Register `state` and produce the cookie for `door_create`.
///
/// The state is already in an `Arc` because a call in flight has to be
/// able to hold it open while another thread revokes.
pub(crate) fn install<T: Send + Sync + 'static>(
    state: Arc<T>,
) -> (Ticket<T>, *mut c_void) {
    let word = slab_install(state);
    let ticket = Ticket {
        word,
        _state: PhantomData,
    };
    (ticket, word as *mut c_void)
}

/// Read a cookie back into live state, if it still is live.
///
/// Returns `None` when the state is gone. The caller MUST treat that
/// as a §3.9 tag `2` reply and MUST NOT fall back to dereferencing the
/// cookie itself.
///
/// A cookie for a door that is gone resolves to `None`. So does a
/// cookie for a slot that has since been reused, and so does a cookie
/// whose state is not a `T`. None of those read the cookie as an
/// address.
///
/// # Safety
///
/// Nothing here is actually unsafe: any machine word may be passed in,
/// because the word is only ever used as a number. The `unsafe` is
/// kept so that the call sites — the trampoline, driven straight from
/// the kernel — still have to say out loud what they are doing.
pub(crate) unsafe fn resolve<T: Send + Sync + 'static>(
    cookie: *mut c_void,
) -> Option<Arc<T>> {
    // No pointer is followed here. The cookie is only ever used as a
    // number, which is why any word is safe to pass in.
    let erased = slab_resolve(cookie as usize)?;
    erased.downcast::<T>().ok()
}

/// [`resolve`], phrased the way the trampoline needs it.
///
/// The trampoline has to answer the caller no matter what, so a
/// missing state is not an early return — it is a reply. This spells
/// the failure as the fault the client will see.
///
/// # Safety
///
/// Same contract as [`resolve`].
pub(crate) unsafe fn resolve_or_fault<T: Send + Sync + 'static>(
    cookie: *mut c_void,
) -> Result<Arc<T>, ServerFault> {
    // SAFETY: we pass the cookie straight through and add no
    // assumptions of our own.
    let state = unsafe { resolve::<T>(cookie) };
    state.ok_or(ServerFault::StateUnavailable)
}

/// Take the state back out. Consumes the ticket.
///
/// Returns the crate's reference to the state. It may not be the last
/// one: a call still running holds a clone, and the state dies when
/// that call finishes.
///
/// Returns `None` only if the ticket was already spent, which the type
/// system makes hard to arrange.
pub(crate) fn uninstall<T: Send + Sync + 'static>(
    ticket: Ticket<T>,
) -> Option<Arc<T>> {
    let erased = slab_uninstall(ticket.word)?;
    erased.downcast::<T>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct State {
        name: String,
    }

    fn state(name: &str) -> Arc<State> {
        Arc::new(State {
            name: name.to_string(),
        })
    }

    #[test]
    fn round_trip() {
        let (ticket, cookie) = install(state("pooled"));

        // SAFETY: the cookie is the one install just gave us.
        let live = unsafe { resolve::<State>(cookie) };
        assert_eq!(live.expect("still installed").name, "pooled");

        let back = uninstall(ticket);
        assert_eq!(back.expect("state comes back").name, "pooled");
    }

    /// The important one. A door that has been revoked leaves its
    /// cookie behind in the kernel; that cookie must answer `None`
    /// rather than point at freed state.
    #[test]
    fn resolve_after_uninstall_is_none() {
        let (ticket, cookie) = install(state("gone"));
        drop(uninstall(ticket));

        // SAFETY: any word is safe to hand to resolve; that is the
        // whole point of a cookie that is a number.
        let dead = unsafe { resolve::<State>(cookie) };
        assert!(dead.is_none(), "a retired cookie must not resolve");
    }

    /// Slot numbers get reused. The generation half of the cookie is
    /// what stops an old cookie from reading the new tenant.
    #[test]
    fn a_reused_slot_ignores_the_old_cookie() {
        let (first, old) = install(state("first"));
        let _ = uninstall(first);

        let (second, fresh) = install(state("second"));

        // SAFETY: resolve accepts any word.
        let stale = unsafe { resolve::<State>(old) };
        assert!(stale.is_none(), "the old cookie must not come back");

        // SAFETY: as above.
        let live = unsafe { resolve::<State>(fresh) };
        assert_eq!(live.expect("the new door works").name, "second");

        let _ = uninstall(second);
    }

    /// The generation is only half a word, so a slot that is built and
    /// revoked often enough runs out of them. When that happens the
    /// slot must be retired, not started over at one: starting over
    /// would make the very first cookies for that slot live again.
    #[test]
    fn a_slot_that_runs_out_of_generations_is_retired() {
        let word = slab_install(state("last"));
        let (index, _) = unpack(word);

        // Fast-forward this slot to its final generation. Only this
        // test knows the index, so no other test can be looking at it.
        {
            let mut slab = write_slab();
            slab.slots[index].generation = INDEX_MASK;
        }

        let back = slab_uninstall(pack(index, INDEX_MASK));
        assert!(back.is_some(), "the state still comes back");

        let slab = read_slab();
        assert_eq!(
            slab.slots[index].generation, 0,
            "a spent slot is marked retired"
        );
        assert!(!slab.free.contains(&index), "and is never handed out again");
    }

    /// A cookie read as the wrong type is a `None`, not a
    /// reinterpretation of somebody else's memory.
    #[test]
    fn a_wrong_type_resolves_to_none() {
        let (ticket, cookie) = install(state("typed"));

        // SAFETY: resolve accepts any word.
        let wrong = unsafe { resolve::<u32>(cookie) };
        assert!(wrong.is_none(), "downcast must refuse another type");

        let _ = uninstall(ticket);
    }

    #[test]
    fn a_dead_cookie_becomes_a_tag_two_fault() {
        let (ticket, cookie) = install(state("fault"));
        let _ = uninstall(ticket);

        // SAFETY: resolve accepts any word.
        let fault = unsafe { resolve_or_fault::<State>(cookie) };
        assert_eq!(fault.unwrap_err(), ServerFault::StateUnavailable);
    }

    #[test]
    fn many_threads_resolve_a_cookie_at_once() {
        let (ticket, cookie) = install(state("shared"));

        // A raw pointer is not `Send`, and it does not need to be:
        // the cookie is a number. Carry the number across.
        let word = cookie as usize;

        let mut threads = Vec::new();
        for _ in 0..8 {
            threads.push(std::thread::spawn(move || {
                for _ in 0..500 {
                    // SAFETY: the ticket is still held on the main
                    // thread, so the slot is still ours.
                    let got = unsafe { resolve::<State>(word as *mut _) };
                    assert_eq!(got.expect("installed").name, "shared");
                }
            }));
        }
        for t in threads {
            t.join().expect("no resolver thread panicked");
        }

        let back = uninstall(ticket);
        let back = back.expect("state comes back");
        assert_eq!(
            Arc::strong_count(&back),
            1,
            "every resolved clone was dropped"
        );
    }
}
