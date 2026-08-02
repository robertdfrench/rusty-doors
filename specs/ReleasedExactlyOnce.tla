-------------------------- MODULE ReleasedExactlyOnce --------------------------
(***************************************************************************)
(* Four different paths in this crate can release a descriptor.  Each one  *)
(* is correct on its own.  The rule they must obey together is that a      *)
(* descriptor is released exactly once, by whoever owns it at that moment. *)
(*                                                                        *)
(* The four paths:                                                        *)
(*                                                                        *)
(*   1. `door_revoke` on owned teardown (`Door::drop`, `Door::revoke`).    *)
(*      door_revoke CLOSES the descriptor.  It is not just an invalidate.  *)
(*      This crate got that wrong once; see `docs/DESIGN.md` Appendix E.   *)
(*      So a revoke IS the release, and nothing may close afterwards.      *)
(*                                                                        *)
(*   2. A plain `close` in the `pthread_atfork` child handler, for a door  *)
(*      the child disowned.  The child MUST NOT revoke.  Revoking would    *)
(*      destroy the door object itself, and the parent is still serving    *)
(*      it.  Only the child's own copy of the number may go away.          *)
(*                                                                        *)
(*   3. The trampoline re-wrapping reply descriptors as `OwnedFd` and      *)
(*      dropping them after a `door_return` that came back.  A door_return *)
(*      that succeeds never comes back at all, and the kernel has taken    *)
(*      the descriptors.  A door_return that fails DOES come back, and it  *)
(*      took nothing -- that is measured, not assumed; see                 *)
(*      `experiments/README.md` and `doors/src/server/trampoline.rs`.      *)
(*                                                                        *)
(*   4. The client re-wrapping `Released` descriptors, on                  *)
(*      `CallError::Rejected` and on nothing else (`GOALS.md` 6.4).  On    *)
(*      success, on `Consumed` and on `Interrupted` the kernel took them,  *)
(*      so closing would be a double close.                                *)
(*                                                                        *)
(* WHY A MODEL AND NOT A TEST                                             *)
(*                                                                        *)
(* Every one of these is easy to read and agree with.  What no test can    *)
(* reach is the moment where two of them meet: a close that is one         *)
(* instruction too late, after the number has already been handed to       *)
(* another thread.  TLC can put the steps in that order on purpose.        *)
(*                                                                        *)
(* WHY A SMALL POOL AND GENERATIONS                                       *)
(*                                                                        *)
(* Descriptor numbers are few and the kernel reuses them.  A model with    *)
(* endless fresh numbers could never say "you closed somebody else's       *)
(* door", because no number would ever come round twice.  So every number  *)
(* carries a generation, bumped each time it is opened.  A close naming    *)
(* the right number but an old generation is a close of another door.      *)
(*                                                                        *)
(* WHY THE BUGGY FLAG                                                     *)
(*                                                                        *)
(* A model that has never failed is worth nothing.  BUGGY puts back one    *)
(* real, plausible defect: the client re-wraps its `Released` descriptors  *)
(* on `CallError::Consumed` as well as on `Rejected`.                      *)
(*                                                                        *)
(* That is plausible because it is the natural belief.  "The call failed,  *)
(* so the kernel did not take my descriptors."  That is true for exactly   *)
(* two errno values, EFAULT and EBADF, and false for every other one.      *)
(* The comment on `classify_call_failure` in `doors/src/client.rs` warns   *)
(* about it in so many words: "Guessing generously here would cause        *)
(* double closes."  A first draft that wrote `_ => Rejected` would look    *)
(* perfectly reasonable and would pass every single-threaded test.         *)
(*                                                                        *)
(* With BUGGY = TRUE, TLC must find a counterexample.  With BUGGY = FALSE  *)
(* it must not.  Running both is what makes the clean run mean anything.   *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS
    Threads,    \* the threads doing the work, deliberately few
    FDs,        \* the descriptor numbers, deliberately few
    MaxGen,     \* how many times a number may be recycled
    BUGGY       \* TRUE makes the client close on Consumed as well

VARIABLES
    fdOpen,     \* fd -> is this number currently open in our table
    fdGen,      \* fd -> which incarnation of the number this is
    fdOwner,    \* fd -> who is allowed to release it
    st,         \* thread -> where it is in its life
    myFd,       \* thread -> the descriptor it is working with
    myGen,      \* thread -> the incarnation it recorded
    fork,       \* "none", "child" while the child handler runs, "done"
    stale,      \* releases of a number that was already gone
    alien,      \* releases that hit a later incarnation: another door
    unowned     \* releases of a live descriptor by someone who does not own it

vars ==
    <<fdOpen, fdGen, fdOwner, st, myFd, myGen, fork, stale, alien, unowned>>

(***************************************************************************)
(* Who can own a descriptor.                                              *)
(*                                                                        *)
(*   FREE     -- the number is not in use.                                *)
(*   KERNEL   -- the kernel has taken it and will close our copy.  This    *)
(*               is what DOOR_RELEASE means, and it is why a client that   *)
(*               closes after a successful call closes twice.              *)
(*   REGISTRY -- a live server door.  The registry is the only place a     *)
(*               live door descriptor goes away, either by door_revoke     *)
(*               (path 1) or by the fork child handler's close (path 2).   *)
(*   a thread -- the thread holds it and must release it itself.           *)
(***************************************************************************)
FREE     == "free"
KERNEL   == "kernel"
REGISTRY == "registry"

Owners == Threads \cup {FREE, KERNEL, REGISTRY}

\* No thread is working with a descriptor.
NONE == 0

States ==
    { "idle",           \* holding nothing
      "door",           \* serving a door this process owns
      "doorForked",     \* a fork just happened; the child handler has not
                        \* reached this entry yet
      "doorDisowned",   \* the child handler released it; we must not
      "reply",          \* holding a descriptor to send back in a reply
      "returnFailed",   \* door_return came back, so we still own them
      "sent",           \* holding a Released descriptor to send in a call
      "rewrap" }        \* the call did not take them, so we take them back

Init ==
    /\ fdOpen  = [f \in FDs |-> FALSE]
    /\ fdGen   = [f \in FDs |-> 0]
    /\ fdOwner = [f \in FDs |-> FREE]
    /\ st      = [t \in Threads |-> "idle"]
    /\ myFd    = [t \in Threads |-> NONE]
    /\ myGen   = [t \in Threads |-> 0]
    /\ fork    = "none"
    /\ stale   = {}
    /\ alien   = {}
    /\ unowned = {}

(***************************************************************************)
(* Releasing a descriptor.  This is the heart of the model.               *)
(*                                                                        *)
(* Four outcomes, in the order they are checked:                          *)
(*                                                                        *)
(*   - the number is not open.  Something released it already.  The        *)
(*     kernel answers EBADF and nothing visible happens, which is why      *)
(*     this kind of bug survives single-threaded testing.                  *)
(*                                                                        *)
(*   - the number is open, but on a later incarnation.  This close shuts   *)
(*     a door that now belongs to somebody else.  This is the dangerous    *)
(*     one.  It is what showed up as fattach failing with EBADF in an      *)
(*     unrelated thread.                                                   *)
(*                                                                        *)
(*   - the number is open, on the right incarnation, but the releaser is   *)
(*     not the owner.  A client closing a descriptor the kernel has taken  *)
(*     lands here.  Real harm follows a moment later, when the kernel      *)
(*     closes its copy too.                                                *)
(*                                                                        *)
(*   - everything matches.  A correct release.                             *)
(*                                                                        *)
(* A wrong release only records itself.  It does not change the table.     *)
(* That keeps the counterexample short: TLC stops at the first bad step    *)
(* instead of following the damage further.                                *)
(***************************************************************************)
Release(f, g, who) ==
    IF ~fdOpen[f]
        THEN /\ stale' = stale \cup {<<f, g, who>>}
             /\ UNCHANGED <<fdOpen, fdOwner, alien, unowned>>
    ELSE IF fdGen[f] # g
        THEN /\ alien' = alien \cup {<<f, g, who>>}
             /\ UNCHANGED <<fdOpen, fdOwner, stale, unowned>>
    ELSE IF fdOwner[f] # who
        THEN /\ unowned' = unowned \cup {<<f, g, who>>}
             /\ UNCHANGED <<fdOpen, fdOwner, stale, alien>>
    ELSE     /\ fdOpen'  = [fdOpen  EXCEPT ![f] = FALSE]
             /\ fdOwner' = [fdOwner EXCEPT ![f] = FREE]
             /\ UNCHANGED <<stale, alien, unowned>>

(***************************************************************************)
(* Getting a descriptor.  The kernel hands back a free number and that     *)
(* number moves on to its next incarnation.                                *)
(*                                                                        *)
(* `own` says who must release it later: REGISTRY for a server door,       *)
(* the thread itself for a reply descriptor or a descriptor about to be    *)
(* sent in a call.                                                         *)
(***************************************************************************)
Acquire(t, own, next) ==
    /\ st[t] = "idle"
    /\ fork # "child"
    /\ \E f \in FDs :
        /\ ~fdOpen[f]
        /\ fdGen[f] < MaxGen
        /\ fdOpen'  = [fdOpen  EXCEPT ![f] = TRUE]
        /\ fdGen'   = [fdGen   EXCEPT ![f] = fdGen[f] + 1]
        /\ fdOwner' = [fdOwner EXCEPT ![f] = own]
        /\ myFd'    = [myFd    EXCEPT ![t] = f]
        /\ myGen'   = [myGen   EXCEPT ![t] = fdGen[f] + 1]
    /\ st' = [st EXCEPT ![t] = next]
    /\ UNCHANGED <<fork, stale, alien, unowned>>

\* door_create.  Ordering against the registry lock is ForkRegistry's
\* subject, so here the door simply belongs to the registry from birth.
BuildDoor(t)  == Acquire(t, REGISTRY, "door")

\* A server procedure produced a descriptor to send back in its reply.
MakeReply(t)  == Acquire(t, t, "reply")

\* A client is about to send SentFd::Released in a door_call.
MakeSend(t)   == Acquire(t, t, "sent")

(***************************************************************************)
(* PATH 1.  Owned teardown.  `Door::drop` and `Door::revoke` ask the      *)
(* registry to release, with revoke = true.  door_revoke closes the        *)
(* descriptor by itself, so this is one release, not a revoke plus a       *)
(* close.  Adding a close here is the bug of Appendix E.                   *)
(***************************************************************************)
TearOwned(t) ==
    /\ st[t] = "door"
    /\ fork # "child"
    /\ Release(myFd[t], myGen[t], REGISTRY)
    /\ st'   = [st   EXCEPT ![t] = "idle"]
    /\ myFd' = [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdGen, myGen, fork>>

(***************************************************************************)
(* fork.  The child handler walks the registry, marks every door disowned  *)
(* and closes it.  Nothing else in the child runs while it does that:      *)
(* after a fork the child has exactly one thread.  `fork = "child"` is     *)
(* that fact, and it keeps the model honest as well as small.              *)
(***************************************************************************)
ForkStart ==
    /\ fork = "none"
    /\ fork' = "child"
    /\ st' = [t \in Threads |->
                IF st[t] = "door" THEN "doorForked" ELSE st[t]]
    /\ UNCHANGED <<fdOpen, fdGen, fdOwner, myFd, myGen, stale, alien, unowned>>

(***************************************************************************)
(* PATH 2.  The child handler releases with a plain close, never a revoke. *)
(* A revoke would destroy the door itself, and the parent is still serving *)
(* on it.  Only this process's copy of the number may go.                  *)
(***************************************************************************)
ChildClose(t) ==
    /\ fork = "child"
    /\ st[t] = "doorForked"
    /\ Release(myFd[t], myGen[t], REGISTRY)
    /\ st' = [st EXCEPT ![t] = "doorDisowned"]
    /\ UNCHANGED <<fdGen, myFd, myGen, fork>>

ForkEnd ==
    /\ fork = "child"
    /\ \A t \in Threads : st[t] # "doorForked"
    /\ fork' = "done"
    /\ UNCHANGED <<fdOpen, fdGen, fdOwner, st, myFd, myGen,
                   stale, alien, unowned>>

\* Dropping a `Door` the child disowned releases nothing at all.  The
\* child handler already did it, and the registry entry's fd was swapped
\* to -1 in the same step, so there is nothing left to close.
TearDisowned(t) ==
    /\ st[t] = "doorDisowned"
    /\ fork # "child"
    /\ st'   = [st   EXCEPT ![t] = "idle"]
    /\ myFd' = [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdOpen, fdGen, fdOwner, myGen, fork, stale, alien, unowned>>

(***************************************************************************)
(* The kernel taking a descriptor.                                        *)
(*                                                                        *)
(* DOOR_RELEASE means the kernel takes the descriptor and closes our copy  *)
(* of the number.  Those are two things, and the gap between them is why   *)
(* the model has a KERNEL owner at all.  In the gap the number is still    *)
(* open and still on the same incarnation, so a wrong close looks          *)
(* completely innocent -- it just is not ours to make.                     *)
(***************************************************************************)
Consume(f) == fdOwner' = [fdOwner EXCEPT ![f] = KERNEL]

\* The kernel gets round to closing our copy.  Now the number is free and
\* another thread may be handed it.
KernelClose ==
    /\ fork # "child"
    /\ \E f \in FDs :
        /\ fdOwner[f] = KERNEL
        /\ fdOpen'  = [fdOpen  EXCEPT ![f] = FALSE]
        /\ fdOwner' = [fdOwner EXCEPT ![f] = FREE]
    /\ UNCHANGED <<fdGen, st, myFd, myGen, fork, stale, alien, unowned>>

(***************************************************************************)
(* PATH 3.  The trampoline's reply descriptors.                           *)
(*                                                                        *)
(* A door_return that succeeds never comes back: control leaves the        *)
(* thread and the stack frame stops existing.  The kernel has the          *)
(* descriptors.  Nothing here can close them, and nothing should.          *)
(***************************************************************************)
DoorReturnOk(t) ==
    /\ st[t] = "reply"
    /\ fork # "child"
    /\ Consume(myFd[t])
    /\ st'   = [st   EXCEPT ![t] = "idle"]
    /\ myFd' = [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdOpen, fdGen, myGen, fork, stale, alien, unowned>>

\* door_return came back, which means it failed.  `experiments/matrix.c`
\* measured every failure reachable from the safe API: in all of them the
\* descriptors were still open and still pointed at the same file.  So we
\* still own them and must close them ourselves.
DoorReturnFailed(t) ==
    /\ st[t] = "reply"
    /\ fork # "child"
    /\ st' = [st EXCEPT ![t] = "returnFailed"]
    /\ UNCHANGED <<fdOpen, fdGen, fdOwner, myFd, myGen, fork,
                   stale, alien, unowned>>

TrampolineClose(t) ==
    /\ st[t] = "returnFailed"
    /\ fork # "child"
    /\ Release(myFd[t], myGen[t], t)
    /\ st'   = [st   EXCEPT ![t] = "idle"]
    /\ myFd' = [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdGen, myGen, fork>>

(***************************************************************************)
(* PATH 4.  The client's Released descriptors.                            *)
(*                                                                        *)
(* Step 1 of GOALS.md 6.4 already happened when the descriptor was made:   *)
(* it is a raw number now, and no OwnedFd for it exists.  What remains is  *)
(* the choice after door_call comes back.                                  *)
(***************************************************************************)

\* EFAULT or EBADF.  The kernel rejected the call before taking anything,
\* so the descriptors are ours to hand back.  This is the only outcome
\* where the crate may close them.
CallRejected(t) ==
    /\ st[t] = "sent"
    /\ fork # "child"
    /\ st' = [st EXCEPT ![t] = "rewrap"]
    /\ UNCHANGED <<fdOpen, fdGen, fdOwner, myFd, myGen, fork,
                   stale, alien, unowned>>

\* Success, or EINTR.  The kernel took them.  Forget the numbers; do not
\* close.
CallTaken(t) ==
    /\ st[t] = "sent"
    /\ fork # "child"
    /\ Consume(myFd[t])
    /\ st'   = [st   EXCEPT ![t] = "idle"]
    /\ myFd' = [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdOpen, fdGen, myGen, fork, stale, alien, unowned>>

(***************************************************************************)
(* Any other errno: CallError::Consumed.  The kernel took them here too.   *)
(*                                                                        *)
(* This is where BUGGY lives.  Under BUGGY the client believes a failed    *)
(* call means it still owns its descriptors, so it goes on to re-wrap and  *)
(* close them.  That belief is right for EFAULT and EBADF and wrong for    *)
(* everything else.                                                       *)
(***************************************************************************)
CallConsumed(t) ==
    /\ st[t] = "sent"
    /\ fork # "child"
    /\ Consume(myFd[t])
    /\ st'   = [st   EXCEPT ![t] = IF BUGGY THEN "rewrap" ELSE "idle"]
    /\ myFd' = IF BUGGY THEN myFd ELSE [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdOpen, fdGen, myGen, fork, stale, alien, unowned>>

ClientRewrap(t) ==
    /\ st[t] = "rewrap"
    /\ fork # "child"
    /\ Release(myFd[t], myGen[t], t)
    /\ st'   = [st   EXCEPT ![t] = "idle"]
    /\ myFd' = [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdGen, myGen, fork>>

Next ==
    \/ \E t \in Threads :
        \/ BuildDoor(t) \/ MakeReply(t) \/ MakeSend(t)
        \/ TearOwned(t) \/ ChildClose(t) \/ TearDisowned(t)
        \/ DoorReturnOk(t) \/ DoorReturnFailed(t) \/ TrampolineClose(t)
        \/ CallRejected(t) \/ CallTaken(t) \/ CallConsumed(t)
        \/ ClientRewrap(t)
    \/ ForkStart
    \/ ForkEnd
    \/ KernelClose

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* What must always hold.                                                 *)
(***************************************************************************)

\* No descriptor is released twice.  The second release finds the number
\* gone.
NoDoubleRelease == stale = {}

\* Nobody releases a number that has moved on to a new incarnation.  That
\* is a close of another door.
NoAlienRelease == alien = {}

\* Nobody releases a descriptor they do not own.
OnlyOwnerReleases == unowned = {}

(***************************************************************************)
(* The other half of "exactly once": nothing is left behind.              *)
(*                                                                        *)
(* If a thread is idle again and a descriptor is still open in its name,   *)
(* that descriptor leaked.  This catches the mirror image of the BUGGY     *)
(* defect -- a client that forgets to hand back a Rejected descriptor, or  *)
(* a trampoline that forgets to close after a failed door_return.          *)
(***************************************************************************)
NoLeak ==
    \A f \in FDs :
        (fdOpen[f] /\ fdOwner[f] \in Threads) => st[fdOwner[f]] # "idle"

TypeOK ==
    /\ fdOpen  \in [FDs -> BOOLEAN]
    /\ fdGen   \in [FDs -> 0..MaxGen]
    /\ fdOwner \in [FDs -> Owners]
    /\ st      \in [Threads -> States]
    /\ fork    \in {"none", "child", "done"}

=============================================================================
