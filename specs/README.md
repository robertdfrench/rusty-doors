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

Each model comes in two configurations: the crate as it is, and the
crate with a real defect put back. The buggy one **must** produce a
counterexample. If it does not, the model is not describing anything and
the clean run means nothing.

`ForkRegistryBuggy` and `ForkRegistryAlien` exist for that reason, not
as documentation of a fix.

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
