# Notes on the `doors` crate, from a consumer

Written for the `rusty-doors` maintainers.

All of this comes from building a real web server on the crate. Six
different ways of streaming Server-Sent Events out of a door, measured
over 101 benchmark runs on OmniOS r151058. None of that matters below,
except that it is an ordinary program doing an ordinary thing, at a
scale that finds problems: 50,000 streams at once, and roughly ten
million door calls.

There are two parts. The first five findings were reported earlier and
have been fixed. We have since moved our code onto those fixes, so this
version also reports how that went. Then there are three new findings.

---

## Part one: the five earlier findings are fixed

Fixed in `6a9a161` and `d57e567`. We checked each against the code
rather than the commit message, then rebuilt our program against the
new crate and ran it.

| # | What it was | Fixed by | How we checked |
|---|---|---|---|
| 1 | `private_pool()` built a door that could not be served | `door_bind` is now called, for private doors only | **Re-ran the test that failed.** It now serves 1,000 streams out of 1,000, and 914,992 events a second. Before, it served two streams and delivered nothing at all. |
| 2 | No shape could return an fd | New `#[door(handback)]` | **Rewrote our server to use it.** All six of our designs still pass end to end. |
| 3 | `DOOR_REFUSE_DESC` and replying with an fd are mutually exclusive | Documented in four places | Read it. There is also a new test, `handback_without_refuse.rs`, pinning the `max_descriptors(0)` workaround. |
| 4 | Nothing could lend its own door | `Door::as_sendable()` | **Now using it.** It replaced an `open` on our own attached path. |
| 5 | Wrong state type gave an error that said nothing | A warning naming both types, once per process | Read it. The error that crosses the wire is one byte and cannot carry a type name, so warning on the server side is the right call. |

### The important one: we no longer touch your internals

Before, our application server had to register a raw C entry point and
call `doors::__private::run` itself, because no shape could return an
fd. Nothing in our tree does that now.

`#[door(handback)]` replaced it exactly. Two raw `extern "C"` functions
and about sixty lines of argument shuffling became two ordinary
methods. The code got shorter and safer at the same time.

That was the most valuable of the five fixes for us. Thank you.

### `as_sendable` works well

We expected trouble here and did not find any.

The door has to be lent from a worker thread, and that thread reaches
it through the same shared state the door itself holds. We thought that
might not be possible. It is. `Door<S>` is both `Send` and `Sync`,
which we confirmed with a compile-time assertion rather than assuming.

The only care needed is to hold a `Weak` rather than an `Arc` in the
door's own state, so the two do not keep each other alive forever. That
is ordinary Rust and not your problem.

Returning a borrow rather than an owned fd is the right choice. It made
it impossible for us to get the lifetime wrong.

### One thing still to consider, on fix 1

The bind result is checked with `debug_assert!`, and the wait for the
door's own fd is bounded, with a comment saying the thread parks
unbound if it never arrives.

Both of those are silent in a release build. The symptom of either is
exactly the bug that was just fixed: a door that answers a call or two
and then stops, with nothing logged anywhere.

Anything at all on that path would turn "broken" into "broken and
traceable". That is the argument the crate makes elsewhere about
failures which arrive as silence, and it applies here too.

---

## Part two: three new findings

### 6. A door you were given cannot be called safely

This is the other half of finding 4, and it is the half still open.

Finding 4 was that a process could not lend its own door. Fixed. But a
process that *receives* a door still cannot call it through the safe
API, because a `Client` can only be built from a path:

```rust
Client::open(path)             // the only way in
Client::open_inheritable(path)
```

A door that arrives in a request arrives as an fd. There is no path. So
the receiver has to drop to `doors-sys` and call `door_call` by hand:

```rust
let mut arg = doors_sys::door_arg_t { /* ... */ };
let r = unsafe { doors_sys::door_call(fd, &mut arg) };
```

This is not as bad as reaching into `__private`. `doors-sys` is a
published crate and this is its stated purpose. But every safety
property the crate exists to provide is gone on that path. We are back
to laying out `door_arg_t` by hand, sizing the reply buffer ourselves,
and remembering that the status byte is there and has to be skipped.

It matters more than it may look, because passing a door in a request
is not exotic. It is how you build anything where the two sides call
each other. For us it is the whole design of the one option that
reaches 50,000 streams.

**What would fix it:** a constructor from a received fd.

```rust
impl Client<NoDescriptors> {
    /// Take ownership of a door that arrived in a request.
    ///
    /// Fails if the fd is not a door.
    pub fn from_received(fd: ReceivedFd) -> Result<Self, Error>;
}
```

`ReceivedFd` already records whether the kernel said the fd is a door,
in `attributes().is_door()`. So the check is available, and the wrong
kind of fd can be refused rather than called.

A borrowing form would help too. The common case is calling the same
door many times without owning it.

### 7. The published documentation cites a file that is not published

There are 44 references to `GOALS.md` in doc comments that ship. They
sit on public items, including this one on the `server` macro, which is
among the first things anybody reads:

```
/// See the [crate docs](crate) for an example, and `GOALS.md` §3 for
/// the full list of `#[door(...)]` options.
```

`GOALS.md` is at the workspace root, not inside `doors/`, so
`cargo publish` does not include it. A reader on docs.rs is sent to a
file they cannot obtain, for the full list of options, which is
something they actually need.

Inside the source these citations are useful and should stay. The ones
in `///` and `//!` comments are a different thing. Those are the
published manual.

Two ways out. Move the substance into the doc comment, so the reader
gets the list instead of a reference to it. Or ship the file. The first
is better: a section number in a design document is not an answer to
"what are my options".

We had this exact problem in our own report, and it was raised by a
reader who could not follow any of it. Citing a document nobody else
has reads as writing for the person who already knows.

### 8. "Shape" is never defined

`shape` is your word for which kind of function a server procedure is.
It appears in the attribute keywords, in error messages, and throughout
the documentation:

```
procedure, rpc, reply_buf, handback, raw
```

We could not find anywhere that says what a shape is. The word is used
as though the reader already knows. We worked it out by reading
`macros/src/options.rs`, which most users will not do.

It reaches users in at least three places: the `#[door(...)]`
attribute, the error when two are given at once, and the sentence
describing the expected signature when the argument count is wrong.

**What would fix it:** one paragraph in the crate documentation, near
the first use, saying what varies between shapes and why there is more
than one. Roughly:

> A server procedure can be written in several forms. They differ in
> what the function takes and returns: raw bytes, a serialised type, a
> buffer to write into, or bytes plus fds. Choose one with
> `#[door(procedure)]`, `#[door(rpc)]`, `#[door(reply_buf)]`,
> `#[door(handback)]` or `#[door(raw)]`. Each generates its own
> `build_<method>()`.

The idea is good and worth keeping. It just needs introducing once.

---

## What worked, unprompted

Worth saying, since everything above is a problem.

- **`Reply` unmapping on `Drop`.** We never thought about it once,
  across 101 runs and roughly ten million calls. No leaks.
- **`SentFd::Shared` genuinely not closing our fd.** Our A4 design
  sends the same door on every one of hundreds of thousands of calls. A
  leak or an early close would have been immediate and obvious.
- **The split between `Rejected` and `Consumed`.** We rely on
  `Rejected` carrying the errno, to reopen and retry when a door server
  restarts. It behaved exactly as documented.
- **Panics contained.** We panicked a server procedure by accident early
  on. The client got an error back. Nothing unwound through a C frame.
- **The stack size check at build time.** It caught a
  `thread_stack_size` that was too small, before it could become a
  crash under load.
- **`#[door(handback)]` refusing a wrong return type with a real
  message.** We wrote `Result<Vec<u8>, E>` and forgot the fds, which is
  exactly the slip that check is there for, and were told so in one
  line.

## Versions

- `doors`, `doors-sys` and `door-macros` as of `d57e567`.
- OmniOS r151058, `SunOS 5.11 omnios-r151058-516f7694c9 i86pc`, two
  CPUs, 2 GB.
- rustc 1.97.1, OmniOS build.

## A note for whoever measures anything here

The first run of a benchmark on a fresh guest was 20 per cent slower
than the three runs after it, on the same binary. Taken alone, it would
have shown a completely fictitious 27 per cent gain from a change that
in fact does nothing at all. Repeat before believing any single number.
