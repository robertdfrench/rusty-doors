// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Somewhere to put reply bytes that never needs freeing.
//!
//! # Why this type exists
//!
//! No value with a destructor may be alive when the trampoline calls
//! `door_return`. That call does not come back when it works, so any
//! clean-up code standing after it simply never runs. A `Vec<u8>`
//! would have to be freed, so a `Vec` cannot be what holds the reply
//! at that moment.
//!
//! [`ReplyBuf`] is the answer. It has no destructor at all. The bytes
//! sit either in an array inside the struct, or in a buffer that
//! belongs to the thread and outlives the call. Neither one has to be
//! given back before `door_return`.

use crate::error::ReplyTooBig;
use std::cell::{Cell, RefCell};
use std::fmt;
use std::ptr;

/// How many bytes fit inside the struct itself.
///
/// 2 KiB. Large enough that most replies never reach for the thread's
/// spill buffer, small enough to sit on a server thread's stack.
const INLINE: usize = 2048;

/// The spill area for one thread.
///
/// `live` is the buffer new writers get. `retired` holds buffers a
/// [`ReplyBuf`] may still be pointing into. See [`Spill::area`] for why
/// old buffers are kept instead of dropped. `lent` says whether some
/// `ReplyBuf` may still be pointing at `live`, which is what stops two
/// of them ever sharing the same bytes.
struct Spill {
    live: RefCell<Vec<u8>>,
    retired: RefCell<Vec<Vec<u8>>>,
    lent: Cell<bool>,
}

/// Allocate `n` zeroed bytes, or `None` if the allocator says no.
///
/// Fallible on purpose. `vec![0u8; n]` panics on a capacity overflow
/// and aborts when the allocator fails, and the limit that sizes it is
/// chosen by the user. A panic here would unwind out of the generated
/// `extern "C"` trampoline, which is undefined behaviour, and it would
/// happen outside the `catch_unwind` that only wraps the user's own
/// function. Refusing the buffer instead turns the same situation into
/// a `ReplyTooBig` reply.
fn zeroed(n: usize) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, 0);
    Some(v)
}

impl Spill {
    /// Hand out a buffer of at least `want` bytes, and how big it is.
    /// `None` means the memory could not be had.
    ///
    /// The same buffer comes back on every call *once the last holder
    /// is gone*, which is the whole point: a server thread pays for
    /// one allocation, not one per door call. See [`reclaim_spill`]
    /// for how the trampoline says the last holder is gone.
    ///
    /// While `lent` is still set the live buffer is retired instead of
    /// reused. Handing the same bytes to a second live `ReplyBuf`
    /// would let each clobber the other's reply, and would let one
    /// write through a raw pointer into memory the other has already
    /// handed out as a `&[u8]` — undefined behaviour reachable from
    /// safe code.
    ///
    /// When a bigger buffer is needed the old one is *retired*, not
    /// resized. Resizing would free the old block, and a `ReplyBuf`
    /// built earlier may still hold a pointer into it. A retired
    /// buffer is never written again and is freed only when the thread
    /// ends, by which time no `ReplyBuf` on that thread can still be
    /// alive. On the trampoline's path nothing is ever retired after
    /// the first call, so the memory kept this way is tiny.
    ///
    /// Every byte is zeroed, so reading back up to the buffer's length
    /// always reads initialised memory.
    fn area(&self, want: usize) -> Option<(*mut u8, usize)> {
        let mut live = self.live.borrow_mut();
        if live.len() < want || self.lent.get() {
            let fresh = zeroed(want)?;
            let old = std::mem::replace(&mut *live, fresh);
            if !old.is_empty() {
                self.retired.borrow_mut().push(old);
            }
        }
        self.lent.set(true);
        Some((live.as_mut_ptr(), live.len()))
    }
}

// The initialiser below is already a `const` block, so there is
// nothing left to make const. Clippy 1.97 reports it anyway: it fires
// on any `thread_local!`, even a one-line `const { Cell::new(0) }`.
// The lint is simply wrong on this toolchain.
thread_local! {
    // The attribute has to sit here, on the static. On the
    // `thread_local!` line the compiler ignores it.
    #[allow(clippy::missing_const_for_thread_local)]
    static SPILL: Spill = const {
        Spill {
            live: RefCell::new(Vec::new()),
            retired: RefCell::new(Vec::new()),
            lent: Cell::new(false),
        }
    };
}

/// Let the thread's spill buffer be lent out again.
///
/// Without this every spilled reply would have to allocate, because
/// [`ReplyBuf`] has no destructor and so cannot give the buffer back
/// by itself.
///
/// # Safety
///
/// No [`ReplyBuf`] made earlier on this thread may still be read or
/// written afterwards. The trampoline calls this on entry, where that
/// holds: a successful `door_return` destroyed the frame every earlier
/// `ReplyBuf` lived in, and it never comes back to one.
pub(crate) unsafe fn reclaim_spill() {
    let _ = SPILL.try_with(|s| s.lent.set(false));
}

/// A place to build a reply, with no clean-up to do afterwards.
///
/// Writing past the limit does not grow the buffer and does not fail
/// loudly. The buffer remembers that it overflowed and keeps counting
/// how many bytes the reply really wanted, so the caller can report
/// the true size. Ask with [`overflow`](ReplyBuf::overflow).
///
/// # Where the bytes live
///
/// [`as_slice`](ReplyBuf::as_slice) points into one of two places.
/// Neither of them is something this buffer owns and must free:
///
/// - **Inline.** Small replies sit in an array inside the struct
///   itself. The trampoline keeps its `ReplyBuf` in its own stack
///   frame, so the bytes are in that frame too. Nothing is allocated
///   and nothing is freed.
/// - **Spilled.** Bigger replies go in a buffer that belongs to the
///   running thread. The thread owns it, reuses it for the next call,
///   and frees it only when the thread itself ends. The `ReplyBuf`
///   only borrows it, and borrows it exclusively: a second `ReplyBuf`
///   that spills while this one is still alive gets a buffer of its
///   own rather than the same bytes.
///
/// This is what makes the pointer safe to use during `door_return`.
/// The kernel reads those bytes while the call is being answered, and
/// on success `door_return` never returns, so no destructor of ours
/// could run to free them anyway. In both modes the memory stays
/// exactly where it is.
///
/// `as_slice` itself only reads fields of the struct. It allocates
/// nothing, locks nothing and cannot panic, so it is safe to call in
/// the last moments before `door_return` where nothing else may be
/// alive.
///
/// # One thread only
///
/// A `ReplyBuf` is neither `Send` nor `Sync`, because a spilled one
/// borrows from the thread that filled it. Fill it and read it on the
/// same thread — which is what a door server procedure does anyway.
///
/// # Example
///
/// ```
/// use doors::server::ReplyBuf;
/// use std::fmt::Write as _;
///
/// let mut out = ReplyBuf::new();
/// out.write_bytes(b"hello ").unwrap();
/// let _ = write!(out, "{}", 42);
/// assert_eq!(out.as_slice(), b"hello 42");
/// assert!(out.overflow().is_none());
/// ```
pub struct ReplyBuf {
    /// Bytes live here while the reply is small.
    inline: [u8; INLINE],
    /// Where the bytes live once they no longer fit inline. Null means
    /// they are still inline. Being a raw pointer is also what keeps
    /// this type off other threads.
    spill: *mut u8,
    /// How big the spill area is. Checked before every write into it.
    spill_cap: usize,
    /// How many bytes are actually stored.
    len: usize,
    /// How many bytes every write asked for, dropped ones included.
    needed: usize,
    /// The cap that refuses to grow.
    limit: usize,
    /// Sticky: at least one write was dropped.
    over: bool,
}

// A destructor here would break `GOALS.md` §4.2 rule 2, and the break
// would only show up as a leak or worse at run time. Fail the build
// instead.
const _: () = assert!(!std::mem::needs_drop::<ReplyBuf>());

impl ReplyBuf {
    /// How many bytes fit without touching the thread's spill buffer.
    pub const DEFAULT_INLINE: usize = INLINE;

    /// The default hard cap on a reply: 64 KiB.
    pub const DEFAULT_LIMIT: usize = 64 * 1024;

    /// An empty buffer with the default limit.
    ///
    /// Touches no thread-local state, so making one is just zeroing a
    /// stack frame.
    pub fn new() -> Self {
        Self::with_limit(Self::DEFAULT_LIMIT)
    }

    /// An empty buffer that refuses to hold more than `limit` bytes.
    ///
    /// A limit below [`DEFAULT_INLINE`](ReplyBuf::DEFAULT_INLINE) is
    /// fine: the buffer simply never spills.
    pub fn with_limit(limit: usize) -> Self {
        ReplyBuf {
            inline: [0u8; INLINE],
            spill: ptr::null_mut(),
            spill_cap: 0,
            len: 0,
            needed: 0,
            limit,
            over: false,
        }
    }

    /// Throw away everything written so far, including a recorded
    /// overflow.
    ///
    /// The thread keeps its spill buffer for the next reply. Only this
    /// `ReplyBuf` forgets about it.
    pub fn clear(&mut self) {
        self.spill = ptr::null_mut();
        self.spill_cap = 0;
        self.len = 0;
        self.needed = 0;
        self.over = false;
    }

    /// How many bytes are stored.
    ///
    /// After an overflow this is smaller than the reply wanted to be.
    /// [`overflow`](ReplyBuf::overflow) has the true size.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether nothing is stored yet.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The hard cap, in bytes.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The bytes written so far.
    ///
    /// See *Where the bytes live* above for what this points into and
    /// why the pointer stays good while `door_return` runs.
    pub fn as_slice(&self) -> &[u8] {
        if self.spill.is_null() {
            return &self.inline[..self.len];
        }
        // SAFETY: `spill` came from the spill area of this thread,
        // which is zeroed to `spill_cap` bytes, so every byte up to
        // `len` is initialised. `store` never lets `len` pass
        // `spill_cap`. The area is never freed or moved while this
        // `ReplyBuf` lives: it belongs to the thread, and a `ReplyBuf`
        // cannot leave the thread that filled it. Growing the area
        // retires the old buffer rather than freeing it. Nothing else
        // writes into it either — `Spill::area` will not lend the same
        // buffer twice, so this slice cannot alias another
        // `ReplyBuf`'s writes.
        unsafe { std::slice::from_raw_parts(self.spill, self.len) }
    }

    /// Append bytes.
    ///
    /// Returns `Err` when the reply would pass the limit. The bytes of
    /// that write are dropped whole, and every later write is dropped
    /// too, but the count of what was wanted keeps rising so the true
    /// size is still known at the end.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), ReplyTooBig> {
        // Saturating, so a silly length cannot wrap the counter and
        // make an overflow look like a fit.
        self.needed = self.needed.saturating_add(bytes.len());

        if self.over || self.needed > self.limit {
            self.over = true;
            return Err(self.too_big());
        }

        if !self.store(bytes) {
            // The thread is shutting down and its spill area is gone.
            // Refusing is the only honest answer; panicking here would
            // unwind out of a trampoline.
            self.over = true;
            return Err(self.too_big());
        }

        Ok(())
    }

    /// The overflow this buffer recorded, if any.
    ///
    /// Sticky: once set it stays set until [`clear`](ReplyBuf::clear).
    /// This is how a caller checks a write that could not report a
    /// failure, such as [`fmt::Write::write_str`].
    pub fn overflow(&self) -> Option<ReplyTooBig> {
        if self.over {
            Some(self.too_big())
        } else {
            None
        }
    }

    fn too_big(&self) -> ReplyTooBig {
        ReplyTooBig {
            needed: self.needed,
            limit: self.limit,
        }
    }

    /// Put `bytes` away. `false` means there was nowhere to put them.
    ///
    /// The caller has already checked that they fit under the limit.
    fn store(&mut self, bytes: &[u8]) -> bool {
        let n = bytes.len();
        if n == 0 {
            return true;
        }
        let end = self.len + n;

        if self.spill.is_null() && end <= INLINE {
            self.inline[self.len..end].copy_from_slice(bytes);
            self.len = end;
            return true;
        }

        if !self.spill_to_fit() || end > self.spill_cap {
            return false;
        }

        // SAFETY: `spill` points at `spill_cap` writable bytes of this
        // thread's spill area, and `end <= spill_cap` was just
        // checked, so the whole copy lands inside it. `bytes` is a
        // separate slice the caller lent us, so the two cannot
        // overlap.
        unsafe {
            ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.spill.add(self.len),
                n,
            );
        }
        self.len = end;
        true
    }

    /// Move to this thread's spill area, carrying the inline bytes
    /// over. `false` means the area could not be reached.
    fn spill_to_fit(&mut self) -> bool {
        if !self.spill.is_null() {
            return true;
        }

        // Ask for the whole limit at once. The area then never has to
        // grow again for this reply, so the pointer stays put for as
        // long as this `ReplyBuf` uses it.
        let want = self.limit;
        let Ok(Some((base, cap))) = SPILL.try_with(|s| s.area(want)) else {
            // Either the thread is tearing its locals down, or the
            // allocator refused a buffer this big. Nothing to spill
            // into. Saying so beats panicking inside a trampoline.
            return false;
        };

        if self.len > 0 {
            if self.len > cap {
                return false;
            }
            // SAFETY: `base` has `cap` writable bytes, `len <= cap`
            // was just checked, and the inline array holds `len`
            // initialised bytes. The two live in different places —
            // one in this struct, one on the heap — so they do not
            // overlap.
            unsafe {
                ptr::copy_nonoverlapping(self.inline.as_ptr(), base, self.len);
            }
        }

        self.spill = base;
        self.spill_cap = cap;
        true
    }
}

impl Default for ReplyBuf {
    fn default() -> Self {
        Self::new()
    }
}

/// Lets `write!` build a reply, which is what the blanket
/// [`ErrorReply`](crate::error::ErrorReply) impl uses to turn a
/// `Display` error into bytes.
///
/// **Never returns `Err` and never panics.** A too-long reply is
/// recorded with [`overflow`](ReplyBuf::overflow) instead. This is
/// deliberate: `write!` turns any `Err` into a panic-shaped mess
/// inside a `Display` impl, and a panic that unwinds out of a
/// trampoline is undefined behaviour. Check `overflow()` after
/// writing.
impl fmt::Write for ReplyBuf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let _ = self.write_bytes(s.as_bytes());
        Ok(())
    }
}

impl fmt::Debug for ReplyBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReplyBuf")
            .field("len", &self.len)
            .field("needed", &self.needed)
            .field("limit", &self.limit)
            .field("spilled", &!self.spill.is_null())
            .field("overflow", &self.over)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn new_is_empty() {
        let out = ReplyBuf::new();
        assert!(out.is_empty());
        assert_eq!(out.len(), 0);
        assert_eq!(out.limit(), ReplyBuf::DEFAULT_LIMIT);
        assert_eq!(out.as_slice(), b"");
        assert!(out.overflow().is_none());
    }

    #[test]
    fn small_writes_stay_inline() {
        let mut out = ReplyBuf::new();
        out.write_bytes(b"hello ").unwrap();
        out.write_bytes(b"world").unwrap();

        assert_eq!(out.as_slice(), b"hello world");
        assert_eq!(out.len(), 11);
        assert!(out.spill.is_null(), "should not have spilled");
        assert!(out.overflow().is_none());
    }

    #[test]
    fn inline_holds_exactly_default_inline() {
        let mut out = ReplyBuf::new();
        let bytes = vec![7u8; ReplyBuf::DEFAULT_INLINE];
        out.write_bytes(&bytes).unwrap();

        assert!(out.spill.is_null(), "should still be inline");
        assert_eq!(out.as_slice(), &bytes[..]);
    }

    #[test]
    fn big_write_spills_and_keeps_the_bytes() {
        let mut out = ReplyBuf::new();
        let bytes: Vec<u8> = (0..5000).map(|i| (i % 251) as u8).collect();
        out.write_bytes(&bytes).unwrap();

        assert!(!out.spill.is_null(), "should have spilled");
        assert_eq!(out.len(), 5000);
        assert_eq!(out.as_slice(), &bytes[..]);
        assert!(out.overflow().is_none());
    }

    #[test]
    fn spilling_carries_the_inline_bytes_over() {
        let mut out = ReplyBuf::new();
        // First write stays inline, second one forces the move.
        let head = vec![1u8; 100];
        let tail = vec![2u8; ReplyBuf::DEFAULT_INLINE];
        out.write_bytes(&head).unwrap();
        assert!(out.spill.is_null());
        out.write_bytes(&tail).unwrap();
        assert!(!out.spill.is_null());

        let mut want = head.clone();
        want.extend_from_slice(&tail);
        assert_eq!(out.as_slice(), &want[..]);
    }

    #[test]
    fn the_thread_buffer_is_reused() {
        // Two replies in a row on one thread should land on the same
        // address: that reuse is the reason the spill path exists.
        // `reclaim_spill` is what the trampoline calls between calls
        // to say the previous reply is gone.
        let big = vec![9u8; 4096];

        let mut first = ReplyBuf::new();
        first.write_bytes(&big).unwrap();
        let a = first.spill;
        // This `drop` frees nothing: `ReplyBuf` has no destructor.
        // It is here to end `first`'s life at this exact point, so the
        // compiler proves nothing reads it after `reclaim_spill`.
        // That is what `reclaim_spill` asks of its caller.
        #[allow(clippy::drop_non_drop)]
        drop(first);

        // SAFETY: `first` is gone and nothing else read it.
        unsafe { reclaim_spill() };

        let mut second = ReplyBuf::new();
        second.write_bytes(&big).unwrap();
        let b = second.spill;

        assert!(!a.is_null());
        assert_eq!(a, b, "the second reply should reuse the buffer");
    }

    #[test]
    fn two_live_buffers_never_share_bytes() {
        // Both spill, both stay alive, neither reclaims. They must get
        // separate memory: sharing would let each clobber the other's
        // reply, and would alias a slice already handed out.
        let mut first = ReplyBuf::new();
        first.write_bytes(&vec![1u8; 4096]).unwrap();
        let seen = first.as_slice().to_vec();

        let mut second = ReplyBuf::new();
        second.write_bytes(&vec![2u8; 4096]).unwrap();

        assert_ne!(first.spill, second.spill, "they must not overlap");
        assert_eq!(first.as_slice(), &seen[..], "the first was clobbered");
        assert_eq!(second.as_slice(), &vec![2u8; 4096][..]);
    }

    #[test]
    fn an_absurd_limit_refuses_instead_of_panicking() {
        // vec![0u8; usize::MAX] panics with "capacity overflow". A
        // panic here would unwind out of an extern "C" trampoline.
        let mut out = ReplyBuf::with_limit(usize::MAX);
        let err = out.write_bytes(&vec![0u8; 4096]).unwrap_err();
        assert_eq!(err.limit, usize::MAX);
        assert!(out.overflow().is_some());
    }

    #[test]
    fn exactly_the_limit_fits() {
        let limit = 4096;
        let mut out = ReplyBuf::with_limit(limit);
        let bytes = vec![3u8; limit];
        out.write_bytes(&bytes).unwrap();

        assert_eq!(out.len(), limit);
        assert_eq!(out.as_slice(), &bytes[..]);
        assert!(out.overflow().is_none());
    }

    #[test]
    fn one_byte_over_the_limit_is_refused() {
        let limit = 4096;
        let mut out = ReplyBuf::with_limit(limit);
        out.write_bytes(&vec![3u8; limit]).unwrap();

        let err = out.write_bytes(b"!").unwrap_err();
        assert_eq!(err.limit, limit);
        assert_eq!(err.needed, limit + 1);
        // The refused byte was not stored.
        assert_eq!(out.len(), limit);
        assert_eq!(out.overflow(), Some(err));
    }

    #[test]
    fn a_small_limit_never_spills() {
        let mut out = ReplyBuf::with_limit(8);
        out.write_bytes(b"12345678").unwrap();
        assert!(out.spill.is_null());
        assert!(out.write_bytes(b"9").is_err());
        assert_eq!(out.as_slice(), b"12345678");
    }

    #[test]
    fn overflow_is_sticky_and_keeps_counting() {
        let mut out = ReplyBuf::with_limit(10);
        out.write_bytes(b"0123456789").unwrap();

        assert!(out.write_bytes(b"abcdefghij").is_err());
        // Later writes are dropped even though this one would fit.
        assert!(out.write_bytes(b"x").is_err());

        let over = out.overflow().expect("overflow was recorded");
        assert_eq!(over.limit, 10);
        assert_eq!(over.needed, 21, "true size, not the stored size");
        assert_eq!(out.len(), 10);
        assert_eq!(out.as_slice(), b"0123456789");
    }

    #[test]
    fn overflow_after_a_spill_still_counts() {
        let mut out = ReplyBuf::with_limit(4096);
        out.write_bytes(&vec![1u8; 3000]).unwrap();
        assert!(!out.spill.is_null());

        assert!(out.write_bytes(&vec![1u8; 2000]).is_err());
        let over = out.overflow().unwrap();
        assert_eq!(over.needed, 5000);
        assert_eq!(over.limit, 4096);
        assert_eq!(out.len(), 3000);
    }

    #[test]
    fn fmt_write_fills_the_buffer() {
        let mut out = ReplyBuf::new();
        write!(out, "{} + {} = {}", 2, 2, 4).unwrap();
        assert_eq!(out.as_slice(), b"2 + 2 = 4");
        assert!(out.overflow().is_none());
    }

    #[test]
    fn fmt_write_reports_overflow_without_failing() {
        let mut out = ReplyBuf::with_limit(4);
        let long = "far too long for four bytes";
        // write! must come back Ok even though nothing fits, or a
        // Display impl would panic inside a trampoline.
        let r = write!(out, "{long}");
        assert!(r.is_ok(), "fmt::Write must never return Err here");

        let over = out.overflow().expect("but it must be recorded");
        assert_eq!(over.limit, 4);
        assert_eq!(over.needed, 27);
    }

    #[test]
    fn clear_resets_everything_including_overflow() {
        let mut out = ReplyBuf::with_limit(8);
        assert!(out.write_bytes(b"much too long").is_err());
        assert!(out.overflow().is_some());

        out.clear();

        assert!(out.overflow().is_none());
        assert!(out.is_empty());
        assert_eq!(out.as_slice(), b"");

        // And it works again afterwards.
        out.write_bytes(b"ok").unwrap();
        assert_eq!(out.as_slice(), b"ok");
        assert!(out.overflow().is_none());
    }

    #[test]
    fn clear_after_a_spill_goes_back_inline() {
        let mut out = ReplyBuf::new();
        out.write_bytes(&vec![5u8; 4096]).unwrap();
        assert!(!out.spill.is_null());

        out.clear();
        assert!(out.spill.is_null());
        assert_eq!(out.len(), 0);

        out.write_bytes(b"tiny").unwrap();
        assert_eq!(out.as_slice(), b"tiny");
    }

    #[test]
    fn empty_writes_are_free() {
        let mut out = ReplyBuf::new();
        out.write_bytes(b"").unwrap();
        assert!(out.is_empty());
        assert!(out.spill.is_null());
    }

    #[test]
    fn a_bigger_limit_grows_the_thread_buffer_safely() {
        // The old buffer is retired, not freed, so the first slice
        // stays readable after the second buffer forces a new
        // allocation.
        let mut small = ReplyBuf::with_limit(4096);
        small.write_bytes(&vec![1u8; 3000]).unwrap();

        let mut large = ReplyBuf::with_limit(512 * 1024);
        large.write_bytes(&vec![2u8; 300_000]).unwrap();

        assert_eq!(small.as_slice(), &vec![1u8; 3000][..]);
        assert_eq!(large.as_slice(), &vec![2u8; 300_000][..]);
    }

    #[test]
    fn buffer_has_no_destructor() {
        assert!(!std::mem::needs_drop::<ReplyBuf>());
    }
}
