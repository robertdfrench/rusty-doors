# Findings against the `doors` crate

Written for the `rusty-doors` maintainers. Everything here came out of
building a real door-based web server against `doors` 0.9.0 — six
variants of a streaming transport, measured over 101 benchmark runs on
an OmniOS r151058 guest. See `README.md` for what that program is; none
of it matters for the report below except that it is an ordinary
consumer of the crate doing an ordinary thing.

Two bugs and three design gaps, in descending order of how much they
cost to work around.

| # | What | Severity |
|---|------|----------|
| 1 | `private_pool()` builds a door that cannot be served | **Bug.** Silent; presents as a hang. |
| 2 | No shape can return a descriptor | Gap. Forces `__private`. |
| 3 | `DOOR_REFUSE_DESC` and descriptor handback are mutually exclusive | Gap. Not documented. |
| 4 | Neither `Door` nor `Client` can lend its descriptor | Gap. Forces `open(2)` on your own path. |
| 5 | `run::<S, …>` must match the builder's `S` or every call fails | Papercut. Error says nothing. |

---

## 1. `DoorBuilder::private_pool()` produces a door that cannot be served

**This is the one worth fixing first.** It fails silently, at run time,
under load, and the symptom points nowhere near the cause.

### What happens

```rust
let mut door = Door::builder(state)
    .request_size(0..=256)
    .private_pool()          // <-- this
    .thread_stack_size(256 * 1024)
    .build(my_proc)?;
door.attach("/tmp/my.door")?;
```

The door attaches. `Client::open` succeeds. The first call or two are
served. Then the door stops answering and callers block in `door_call`
indefinitely.

Measured, with the web server making one door call per HTTP request —
so "streams established" is the number of door calls that returned:

| | shared pool | `private_pool()` |
|---|---|---|
| connections opened | 1,000 | 1,000 |
| **streams established** | **1,000** | **2** |
| events delivered in 20 s | 10,137,642 | **0** |

The same shape for a door called once per *event* rather than once per
stream (our A4 variant): 1,402,985 events against **0**, and again
exactly two streams established.

Both processes were healthy throughout and shut down normally. Nothing
logged an error. The door was simply not answering.

### Why

`door_create(3C)` with `DOOR_PRIVATE` gives the door its own pool of
server threads. A thread joins a *private* pool by calling
`door_bind(3C)` with that door's descriptor and *then* parking in
`door_return(NULL, 0, NULL, 0)`. A thread that parks without binding
joins the **process-wide** pool instead.

`create_server_thread` in `doors/src/server/builder.rs` parks without
binding:

```rust
unsafe extern "C" fn create_server_thread(info: *mut door_info_t) {
    // ...
    let _ = std::thread::Builder::new()
        .stack_size(stack)
        .name(String::from("door-server"))
        .spawn(move || {
            unsafe {
                sys::pthread_setcancelstate(PTHREAD_CANCEL_DISABLE,
                                            std::ptr::null_mut());
            }
            // Join the door's pool of waiting threads. ...
            unsafe {
                sys::door_return(std::ptr::null(), 0,
                                 std::ptr::null(), 0);   // <-- no door_bind
            }
        });
}
```

The comment says "join the door's pool". It joins *a* pool — the global
one. So a `DOOR_PRIVATE` door receives none of the threads created for
it, and is served only by whatever thread happened to be bound when it
was created. That is where the "2" in the table comes from.

`door_bind` is already declared in `doors-sys`:

```
doors-sys/src/ffi.rs:117:  /// See [`door_bind(3C)`][1].
```

and has **no caller anywhere in the safe layer**:

```console
$ grep -rn door_bind doors/src/
$ echo $?
1
```

### The information the creation function needs is already there

`create_server_thread` receives `*mut door_info_t`, and `di_data` is
already read out of it to look up the stack size. The same cookie could
carry the door's descriptor, which is what `door_bind` wants.

### Suggested fixes, cheapest first

1. **Make `private_pool()` refuse to build.** One line, honest, and
   strictly better than the status quo: a compile-or-build-time error
   beats a hang under load. Document that private pools are not
   supported yet.
2. **Bind.** Thread the door's descriptor through the cookie and call
   `door_bind(fd)` before `door_return` when the door has
   `DOOR_PRIVATE` set. Note that a bound thread serves only that door,
   so a process with both private and shared doors needs the creation
   function to distinguish them — which the `door_info_t` argument
   makes possible.
3. Either way, `experiments/` would be the natural home for a small C
   program that demonstrates the difference, in the style of
   `servercreate.c`.

### Reproducing it

Any door built with `.private_pool()` and driven with more concurrency
than one call at a time. In this repository:

```sh
cd ~/portunusd
./target/release/appserver --variant A0 --private-pool \
    --door /tmp/portunusd.door &
./target/release/webserver --variant A0 --door /tmp/portunusd.door \
    --port 8080 &
./target/release/loadgen --host 127.0.0.1 --port 8080 --streams 1000 \
    --duration-s 20 --out /tmp/load.json
# events: 0; the web server reports "teardown of 2 streams"
```

---

## 2. No `#[doors::server]` shape can return a descriptor

`Outcome` has the field:

```rust
pub struct Outcome<E> {
    pub data: Result<Vec<u8>, E>,
    /// Descriptors to send back. The four shapes in §3.3 never set
    /// this, but the trampoline handles it so that rule 4.2.4 is
    /// implemented once, here, rather than in each future shape.
    pub descriptors: Vec<OwnedFd>,
}
```

The trampoline handles reply descriptors properly, including the
rule-4.2.4 re-wrap on the paths where `door_return` comes back. But
every one of the four generated shapes calls `Outcome::bytes`, which
hard-codes `descriptors: Vec::new()`, and `Outcome` is only reachable
through `doors::__private`.

So a server that hands a descriptor back — which is the whole design of
the thing we were building, and a normal use of doors generally — has
to register a raw `extern "C"` procedure and call
`doors::__private::run` itself:

```rust
unsafe extern "C" fn stream_proc(
    cookie: *mut c_void, argp: *mut c_char, arg_size: usize,
    dp: *mut doors::__private::door_desc_t, n_desc: c_uint,
) {
    run::<Arc<App>, NoDescriptors, _, io::Error>(
        cookie, argp, arg_size, dp, n_desc, 4096, ReplyProtocol::Tagged,
        |app: &Arc<App>, req: Request<'_, NoDescriptors>| {
            Outcome { data: Ok(reply_bytes), descriptors: vec![the_fd] }
        },
    )
}
```

`__private` is documented as "not a public API" and "not covered by
semantic versioning", so this is a program pinned to a patch release of
the crate for want of a shape.

**Suggested fix:** a fifth shape. The signature that would have covered
every case we hit:

```rust
#[door(handback)]
fn stream(&self, req: Request<'_, D>)
    -> Result<(Vec<u8>, Vec<OwnedFd>), E>
```

Everything underneath it already exists.

---

## 3. `DOOR_REFUSE_DESC` and descriptor handback cannot be combined

Our application's door does not *accept* descriptors — only one of six
variants sends one — so `refuse_descriptors()` is exactly what we
wanted. It cannot be used, because it makes the reply unreachable.

`Client::with_descriptors()` checks `DOOR_REFUSE_DESC` and refuses:

```rust
pub fn with_descriptors(self) -> Result<Client<Descriptors>, Error> {
    let info = self.info()?;
    if info.attributes() & DOOR_REFUSE_DESC != 0 {
        return Err(Error::RefusesDescriptors);
    }
    // ...
}
```

and `with_descriptors()` is also what makes a client able to **receive**
one. A `Client<NoDescriptors>` handed a descriptor closes it and fails
the call with `UnexpectedDescriptors`.

So `DOOR_REFUSE_DESC` — a flag about the *argument* direction — makes
the *reply* direction unusable through the safe API. We fell back to
`max_descriptors(0)`, which keeps the part of the intent that matters
(the kernel rejects a call carrying a descriptor before the server
procedure runs) without the flag.

Two related notes:

- The typestate reads as being about sending. `Client<NoDescriptors>`
  sounds like "this client does not send descriptors"; it also means
  "this client cannot receive one". Every one of our five handback
  variants needs `with_descriptors()` despite none of them ever sending
  anything. Not wrong — one flag governs both directions — but the name
  points one way and the consequence points the other, and the failure
  arrives as *every request failing at run time* rather than as a
  compile error.
- Worth a sentence in the `refuse_descriptors()` and
  `with_descriptors()` docs: "a door that returns descriptors must not
  set this."

---

## 4. Neither `Door` nor `Client` will lend its descriptor

This is deliberate and documented, and we think it is the right default
— handing the raw descriptor out would let somebody `close` it or
`door_call` it behind the typestate's back. Recording it because it has
a cost that is not obvious until you hit it.

To send *your own* door to a peer, you need a descriptor for it. There
is no way to get one from the `Door` you are holding, so you have to
open your own `fattach`ed path:

```rust
let mut d = Door::builder(state).build(emit_proc)?;
d.attach(&path)?;
// ...and now, to send it, open it again by name:
let raw = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
let fd = unsafe { OwnedFd::from_raw_fd(raw) };
client.call_with_descriptors(data, vec![SentFd::Shared(fd.as_fd())])?;
```

That works, but it means the door can only be sent if it has been
attached to a path — a `door_create`d door that was never `fattach`ed
cannot be handed to anybody, even though the kernel is perfectly happy
to pass it.

**Suggested fix:** a borrowing accessor that yields something sendable
without yielding something closable, e.g.

```rust
impl<S> Door<S> {
    /// Borrow this door for sending to a peer.
    pub fn as_sendable(&self) -> SentFd<'_>;
}
```

---

## 5. `run::<S, …>` must match the builder's `S`, and says nothing if it does not

```rust
let door = Door::builder(app.clone())        // S = Arc<App>
    .build(stream_proc)?;

// in stream_proc:
run::<App, NoDescriptors, _, io::Error>(...)  // S = App  <-- wrong
```

This compiles. Every call then fails with
`ServerFailed(StateUnavailable)`, because the cookie registry is keyed
by type and the lookup misses.

It took a while to find, because `StateUnavailable` reads as "the
server's state has gone away" — which is what it means, but the reason
here is that it was never registered under the type being asked for.
The error cannot distinguish "your state was dropped" from "you asked
for the wrong type".

This only bites people using `run` directly, which today means people
working around finding #2 — so fixing #2 largely removes it. If the raw
path is going to stay reachable, it would help for
`Error::ServerFault::StateUnavailable` to carry the type name it looked
for (`std::any::type_name::<S>()`), which is free in the failure path.

---

## Things that worked exactly as documented

Worth saying, since the above is all complaints.

- **`Reply` unmapping on `Drop`.** Never thought about it once across
  101 runs and millions of calls. No leaks.
- **The `Rejected { returned, .. }` / `Consumed` split.** We rely on
  `Rejected` carrying the errno to implement the `EBADF` → reopen retry
  that GOALS.md asks for, and on `Released` descriptors coming back on
  that path. Both behaved.
- **`SentFd::Shared` genuinely not closing our descriptor.** The A4
  variant sends the same door on every one of hundreds of thousands of
  calls. If `Shared` had leaked or closed, we would have found out
  immediately.
- **Panic containment.** We panicked a server procedure by accident
  early on and got an error back at the client rather than an
  unwind through an `extern "C"` frame.
- **The stack-size check at build time.** Caught a genuinely too-small
  `thread_stack_size` before it became a run-time crash.

## Versions

- `doors` 0.9.0, `doors-sys` 0.1.0, `door-macros` 0.2.0, as of the
  worktree next to this one.
- OmniOS r151058 (`SunOS 5.11 omnios-r151058-516f7694c9 i86pc`),
  2 vCPUs, 2 GB.
- rustc 1.97.1 (OmniOS/151058).

---

# Response from the maintainers

All five confirmed against `doors` 0.9.0 and fixed. Thank you — the
report was accurate in every particular, and finding 1's diagnosis was
correct down to the mechanism.

| # | Status | Where |
|---|---|---|
| 1 | Fixed | `door_bind` before parking, private doors only |
| 2 | Fixed | new `#[door(handback)]` shape |
| 3 | Documented | four doc sites, plus a test pinning the workaround |
| 4 | Fixed | `Door::as_sendable()` |
| 5 | Improved | a once-per-process warning naming both types |

**1.** Exactly as you diagnosed. `experiments/private_pool.c` shows it
in C — the same program hangs without the bind and answers 20 of 20
with it. The fix binds only `DOOR_PRIVATE` doors, because a bound
thread serves that door and nothing else; binding a shared one would
have starved every other door in the process.

There was a wrinkle from inside: for a private door the creation
function can be called *during* `door_create`, before there is a
descriptor to bind to. The per-door table now carries the descriptor,
the builder publishes it as soon as it has one, and the server thread
waits on a condvar — bounded, because trading a hang under load for a
hang at startup would be no improvement.

`doors/tests/private_pool.rs` is the regression test. It discriminates:
with the bind removed the two private-pool cases fail and the
shared-pool case still passes.

**2.** `#[door(handback)]`, with the signature you proposed. Note the
reply carries at most 16 descriptors (`MAX_REPLY_DESCRIPTORS`) and
anything past that is closed rather than sent — documented on the
shape, since losing a descriptor silently would be a poor trade for the
`__private` escape you were making.

**3.** Documentation only; the behaviour is right and you said so. The
trap is now spelled out on `refuse_descriptors`, `max_descriptors`,
`with_descriptors` and both marker types, and
`doors/tests/handback_without_refuse.rs` pins the `max_descriptors(0)`
workaround so it cannot regress.

**4.** `Door::as_sendable() -> Result<SentFd<'_>, Error>`. It returns
`Result` rather than a bare `SentFd` so a door disowned by a `fork` can
refuse, consistent with `detach` and `info`.

**5.** With a constraint you could not have seen: `ServerFault` crosses
the wire as a single discriminant byte, so it cannot carry a type name
to the client. Instead the server prints once per process, naming both
the type the state was registered as and the type the procedure asked
for. Once, not per call, and through `write_all` rather than
`eprintln!` — the latter panics if the write fails, and a panic there
would unwind into the kernel's frame.

Your "things that worked" section was the most useful part to receive.
`Reply`'s unmapping, the `Rejected`/`Consumed` split and
`SentFd::Shared` had never been exercised at that volume here.
