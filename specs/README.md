# Specs

TLA+ models of the parts of this crate where the danger is *interleaving*
and testing cannot reach.

Run one with:

    make ForkRegistry.check

## Why these and not others

Most of the bugs found while writing this crate came from a wrong belief
about illumos, not a wrong algorithm: `door_revoke` closes the
descriptor, `door_xcreate` hands you a pointer into a dying stack frame,
`door_return` can fail without setting errno. A model inherits your
assumptions, so it would have verified all of those as correct.

What a model is good for here is the opposite case: a rule that is
written down, believed, and impossible to demonstrate. You cannot drive
a `fork` to land between two particular instructions on another thread,
and you cannot make the kernel hand out a descriptor number at the exact
moment that exposes a race. TLC can.

## A spec that has never failed is worth nothing

Each model comes in at least two configurations: the crate as it is, and
the crate with a real defect put back. The buggy one **must** produce a
counterexample. If it does not, the model is not describing anything and
the clean run means nothing.

`ForkRegistryBuggy` and `ForkRegistryAlien` exist for that reason, not
as documentation of a fix.

## One model, several configurations

TLC reads the `.cfg` file whose name matches the module, so every
configuration needs a module of its own. Those extra modules are one
line each: `EXTENDS <the model>`. The model itself lives in exactly one
file. A copied model drifts, and once it has drifted the clean run and
the failing run are no longer describing the same thing.

## ForkRegistry

GOALS.md §7.1: take the lock, remove the entry, release the descriptor,
release the lock — in that order.

Descriptor numbers are a small pool and the kernel reuses them, always
returning the lowest free one. So each number carries a *generation*,
bumped on every open. A close naming the right number but the wrong
generation is a close of somebody else's door. A model with unlimited
descriptors could not express that, because no number would ever be
handed out twice — which is exactly why the real bug was invisible in
single-threaded tests.

| config | checks | result |
|---|---|---|
| `ForkRegistry.cfg` | all invariants, `BUGGY = FALSE` | 1742 states, clean |
| `ForkRegistryBuggy.cfg` | all invariants, `BUGGY = TRUE` | `RegistryHonest` violated |
| `ForkRegistryAlien.cfg` | `NoAlienClose` only, `BUGGY = TRUE` | violated in 111 states |

The third is the interesting one. Its counterexample is the defect this
crate shipped:

```
alien  = {<<3, 1>>}      t1 closed descriptor 3, generation 1
fdGen  = (3 :> 2)        but 3 is on generation 2 now
myFd   = (t2 :> 3)       and that generation belongs to t2
```

`Door::drop` called `door_revoke` outside the registry lock, and
`door_revoke` closes the descriptor. Then `deregister_and_close` took
the lock and closed the same number again. In the gap, another thread
called `door_create` and was handed it.

In the real system this took a reproducer, `truss`, and two wrong fixes
to find. TLC finds it in 111 states.

## ReleasedExactlyOnce

Four different paths in this crate can release a descriptor:

1. `door_revoke` on owned teardown — `Door::drop` and `Door::revoke`.
   `door_revoke` **closes** the descriptor. It is not an invalidate.
   That is the fact the crate got wrong once (Appendix E), so the revoke
   *is* the release and nothing may close after it.
2. A plain `close` in the `pthread_atfork` child handler, for a door the
   child disowned. The child must **not** revoke: revoking destroys the
   door object, and the parent is still serving on it.
3. The trampoline re-wrapping reply descriptors as `OwnedFd` and
   dropping them after a `door_return` that came back. A `door_return`
   that succeeds never comes back and the kernel has taken them; one
   that fails took nothing. That is measured, not assumed —
   `experiments/matrix.c`.
4. The client re-wrapping `Released` descriptors on `CallError::Rejected`
   and on nothing else (`GOALS.md` §6.4). On success, on `Consumed` and
   on `Interrupted` the kernel took them.

Each rule is easy to read and easy to agree with. What no test can reach
is the moment where two of them meet: a close one instruction too late,
after the number has been handed to another thread. So the model has a
small pool of numbers, each carrying a generation, exactly as
`ForkRegistry` does.

The model also gives the kernel its own ownership state. `DOOR_RELEASE`
means the kernel takes the descriptor and closes our copy of the number,
and those are two things. Inside the gap the number is still open and
still on the same generation, so a wrong close looks completely
innocent — it simply is not ours to make.

### The defect put back

`BUGGY` makes the client re-wrap its `Released` descriptors on
`CallError::Consumed` as well as on `Rejected`.

That is the natural belief: *the call failed, so the kernel did not take
my descriptors*. It is true for exactly two errno values, `EFAULT` and
`EBADF`, and false for every other one. The comment on
`classify_call_failure` in `doors/src/client.rs` warns about it in so
many words — "Guessing generously here would cause double closes" — and
a first draft that wrote `_ => Rejected` would look perfectly reasonable
and pass every single-threaded test.

| config | checks | result |
|---|---|---|
| `ReleasedExactlyOnce.cfg` | all invariants, `BUGGY = FALSE` | 2915 states, clean |
| `ReleasedExactlyOnceBuggy.cfg` | all invariants, `BUGGY = TRUE` | `OnlyOwnerReleases` violated in 154 states |
| `ReleasedExactlyOnceAlien.cfg` | `NoAlienRelease` only, `BUGGY = TRUE` | violated in 1078 states |

The buggy run stops at the first bad step, three moves in:

```
Acquire(t1, t1, "sent")     a client takes fd 3 to send as Released
CallConsumed(t1)            door_call fails with some other errno;
                            the kernel took fd 3
ClientRewrap(t1)            the client closes it anyway

unowned = {<<3, 1, t1>>}    t1 released fd 3, generation 1
fdOwner = (3 :> "kernel")   but the kernel owns it now
fdOpen  = (3 :> TRUE)       and it is still open, so nothing looks wrong
```

The third configuration is the interesting one. Asking only about the
worst outcome makes TLC walk past the mild symptoms and keep going until
the number has been reused:

```
Acquire(t1, t1, "sent")            client takes fd 3, generation 1
CallConsumed(t1)                   the kernel takes fd 3
KernelClose                        the kernel closes our copy; 3 is free
Acquire(t2, "registry", "door")    t2 calls door_create and is handed 3
ClientRewrap(t1)                   t1 closes fd 3 at last

alien   = {<<3, 1, t1>>}      t1 closed fd 3, generation 1
fdGen   = (3 :> 2)            but 3 is on generation 2 now
fdOwner = (3 :> "registry")   and generation 2 is t2's live server door
```

That is the same ending as Appendix E, reached down a completely
different path. One client's wrong reading of one errno takes down
another thread's door.

### The other half of "exactly once"

`NoLeak` says no descriptor may still be open in an idle thread's name.
It catches the mirror-image defect: a client that forgets to hand back a
`Rejected` descriptor, or a trampoline that forgets to close after a
failed `door_return`. Removing the re-wrap step from `CallRejected`
makes TLC report `NoLeak` at once, so the invariant is not decoration.

## RevokeDrain

`GOALS.md` §5.1 says `Door::revoke()` revokes, waits for the in-flight
counter to reach zero, drops the state and returns it. Two things were
known going in, and neither is assumed by the model:

- **`in_flight` is never incremented.** Only the initialiser and the
  load in `Door::revoke` exist, so the drain loop reads zero and returns
  at once.
- There is a window between the kernel entering the trampoline and the
  trampoline raising the counter. A revoke that reads the counter inside
  that window sees zero while a call is about to resolve the cookie.
  Whether that matters was an open question (`docs/DESIGN.md` E.1
  item 1).

The model has server threads handling calls, one revoking thread, the
counter, and the lifetime of the state. It also has the slab, because
the slab is what decides whether a late cookie resolves at all. There is
exactly **one** slab slot and it is reused, with a generation stamp — if
every door had a fresh slot, a stale cookie could never meet a new
tenant and the generation would never be tested.

Three knobs:

- `INCREMENT` — where the counter goes up relative to resolving the
  cookie: `"none"` (today), `"after"`, `"before"`.
- `ARCCLONE` — `TRUE` when resolving clones an `Arc` under the slab
  lock, so an in-flight call holds its own reference. That is the crate
  today. `FALSE` is the old `Pinned` world, where the drain was the only
  thing keeping the state alive. `FALSE` is the setting that says what
  the drain is worth **on its own**.
- `ORDER` — `"drainThenUninstall"` is what `Door::revoke` does now: read
  the counter, then take the state out of the slab.
  `"uninstallThenDrain"` is the other way round.

Two invariants. `NoUseAfterFree` is memory safety: the state is never
freed while a call sits between resolving the cookie and returning.
`RevokeGetsOwned` is the shape of the result: `revoke()` hands back an
owned `S` rather than `Err(StateStillShared)`.

| config | `INCREMENT` | `ARCCLONE` | `ORDER` | result |
|---|---|---|---|---|
| `RevokeDrain.cfg` | before | TRUE | uninstall first | 560 states, clean |
| `RevokeDrainOrder.cfg` | before | FALSE | uninstall first | 560 states, clean |
| `RevokeDrainToday.cfg` | none | TRUE | drain first | `RevokeGetsOwned` violated in 58 states |
| `RevokeDrainNoCounter.cfg` | none | FALSE | drain first | `NoUseAfterFree` violated in 58 states |
| `RevokeDrainAfter.cfg` | after | FALSE | drain first | `NoUseAfterFree` violated in 68 states |
| `RevokeDrainBefore.cfg` | before | FALSE | drain first | `NoUseAfterFree` violated in 99 states |
| `RevokeDrainRace.cfg` | before | TRUE | drain first | `RevokeGetsOwned` violated in 99 states |

Five of the seven fail. The two that pass pass for a reason the failing
five make visible.

### (a) No increment at all

`RevokeDrainNoCounter.cfg`. The drain is the only protection and it does
nothing:

```
Dispatch(t1)      the kernel enters the trampoline
Resolve(t1)       t1 gets the state and starts the user function
RvRevoke          door_revoke
RvDrain           the counter reads 0, so the loop exits at once
RvUninstall       the state leaves the slab
RvUnwrap          the state is freed

ph    = (t1 :> "running")   t1 is still inside the user function
held  = (t1 :> 1)           still holding state 1
freed = <<TRUE, FALSE>>     which has just been freed
```

### (b) Increment after resolving the cookie

`RevokeDrainAfter.cfg`. Same six steps, and the counter is never raised
at all: revoke fits entirely into the gap between resolving the cookie
and the `fetch_add` on the next line. The final state differs only in
`ph = (t1 :> "resolved")`. **Yes, it still fails.**

### (c) Increment before resolving the cookie

`RevokeDrainBefore.cfg`. This one was expected to hold. It does not:

```
Dispatch(t1)      the kernel enters the trampoline; nothing done yet
RvRevoke          door_revoke
RvDrain           the counter reads 0, so the loop exits
RaiseBefore(t1)   NOW t1 raises the counter
Resolve(t1)       the state is still in the slab, so t1 gets it
RvUninstall       revoke takes the state out
RvUnwrap          and frees it

inFlight = 1                the counter says one call is in flight
ph       = (t1 :> "running")
held     = (t1 :> 1)
freed    = <<TRUE, FALSE>>  and the state is gone anyway
```

Moving the increment earlier does not close the window. It moves it. The
drain reads the counter and then does something else, and a call can
arrive in between. Reading zero means "zero a moment ago", and revoke
acts on it later.

### The ordering that does work

`RevokeDrainOrder.cfg` — same increment placement, same absence of any
`Arc`, but the state leaves the slab **before** the counter is read.
Clean, 560 states, and this is the answer to the question:

- Take the state out of the slab first. From that moment no cookie can
  resolve, so no new call can take the state.
- Then drain. Every call that did take it raised the counter first, so
  the counter cannot read zero until they are all done.
- The two halves are both needed. `INCREMENT = "none"` with
  `"uninstallThenDrain"` still violates `NoUseAfterFree`, because a call
  that resolved before revoke started is invisible.

There is one more ordering rule inside the trampoline, and the model
enforces it as two separate steps: the state must be let go **before**
the counter comes down. If the counter went down first, a drain could
read zero while a clone was still alive.

### Is the drain needed for safety at all?

**No.** `RevokeDrainToday.cfg` is the crate exactly as it stands — no
increment, drain first — and `NoUseAfterFree` holds through the whole
state space. Resolving the cookie clones an `Arc` under the slab lock,
in one step, so "is the state still there?" and "take a reference to it"
cannot come apart. An in-flight call holds its own reference and the
state cannot be freed underneath it. The missing counter is not a
memory-safety bug.

What the missing counter costs is the result:

```
Dispatch(t1)      the kernel enters the trampoline
Resolve(t1)       t1 resolves the cookie and clones the Arc
RvRevoke          door_revoke
RvDrain           the counter reads 0
RvUninstall       the state leaves the slab
RvUnwrap          Arc::try_unwrap -- and finds refs = 1

shared = TRUE               revoke() returns Err(StateStillShared)
refs   = <<1, 0>>           because t1 still holds its clone
freed  = <<FALSE, FALSE>>   nothing was freed; this is not unsafe
```

This settles `docs/DESIGN.md` E.1 item 1. The drain is **not** a
soundness mechanism any more. It is only the thing that lets `revoke()`
hand back an owned `S`, exactly as E.1 says.

### And the drain as written cannot even do that

`RevokeDrainRace.cfg` puts the increment in the best place E.1 asks for
— before resolving the cookie — and keeps the `Arc` and the current
drain-then-uninstall order. `RevokeGetsOwned` still fails, in the same
eight steps as (c) above: the drain reads zero, a thread that was
already inside the trampoline then increments and resolves, and
`try_unwrap` finds `refs = 1`.

So adding the increment on its own does not deliver what §5.1 promises.
`RevokeDrain.cfg` — increment before the resolve, **and** uninstall
before the drain — is the configuration where both invariants hold. In
`Door::revoke` that means moving `cookie::uninstall` above the drain
loop, and doing `Arc::try_unwrap` after it.
