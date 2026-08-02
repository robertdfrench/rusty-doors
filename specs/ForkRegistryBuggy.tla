------------------------------ MODULE ForkRegistryBuggy ------------------------------
(***************************************************************************)
(* The door registry, and the rule that a descriptor is released exactly    *)
(* once.                                                                    *)
(*                                                                          *)
(* `doors` keeps every server door in a process-global registry so that a   *)
(* fork's child handler can disown and close them.  GOALS.md 7.1 states     *)
(* the ordering rule:                                                       *)
(*                                                                          *)
(*     take the lock, remove the entry, release the descriptor,             *)
(*     release the lock -- in that order.                                   *)
(*                                                                          *)
(* The reason given is that closing outside the lock would let another      *)
(* thread be handed that descriptor number first, so the close would shut   *)
(* somebody else's door.  That is a claim about interleavings, and it       *)
(* cannot be tested: you cannot drive a close to land between two           *)
(* particular instructions on another thread.  So it is modelled here.      *)
(*                                                                          *)
(* WHY GENERATIONS                                                          *)
(*                                                                          *)
(* Descriptor numbers are a small pool and the kernel reuses them, always   *)
(* handing back the lowest free one.  A model with unlimited descriptors    *)
(* could never express "closed the wrong door", because no number would     *)
(* ever be handed out twice.  So each descriptor carries a generation,      *)
(* bumped every time it is opened.  A close that names the right number     *)
(* but the wrong generation is a close of somebody else's door.             *)
(*                                                                          *)
(* WHY THE BUGGY FLAG                                                       *)
(*                                                                          *)
(* A specification that passes tells you nothing until you have watched it  *)
(* fail.  BUGGY reintroduces the real defect this crate shipped: teardown   *)
(* called door_revoke, which closes the descriptor, and then closed it      *)
(* again.  With BUGGY = TRUE, TLC must find a counterexample.  With         *)
(* BUGGY = FALSE it must not.  Checking both is what makes the clean run    *)
(* mean something.                                                          *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS
    Threads,    \* the threads building and tearing down doors
    FDs,        \* the descriptor numbers, deliberately few
    MaxGen,     \* how many times a number may be recycled
    BUGGY       \* TRUE reintroduces the historical double close

VARIABLES
    lock,       \* "free", or the thread or "prepare" that holds it
    fdOpen,     \* fd -> is this number currently open
    fdGen,      \* fd -> which incarnation it is on
    reg,        \* the registry: a set of [fd, gen] entries
    st,         \* thread -> where it is in its life
    myFd,       \* thread -> the descriptor it is working with
    myGen,      \* thread -> the incarnation it recorded
    forkDone,   \* has the child handler run
    stale,      \* closes of an already-closed number  (EBADF, mild)
    alien       \* closes of somebody else's door      (the real danger)

vars == <<lock, fdOpen, fdGen, reg, st, myFd, myGen, forkDone, stale, alien>>

NONE == 0

Init ==
    /\ lock = "free"
    /\ fdOpen = [f \in FDs |-> FALSE]
    /\ fdGen = [f \in FDs |-> 0]
    /\ reg = {}
    /\ st = [t \in Threads |-> "idle"]
    /\ myFd = [t \in Threads |-> NONE]
    /\ myGen = [t \in Threads |-> 0]
    /\ forkDone = FALSE
    /\ stale = {}
    /\ alien = {}

(***************************************************************************)
(* Releasing a descriptor.  This is the whole point of the model.          *)
(*                                                                         *)
(*   - the number is open and the generation matches: a correct close.     *)
(*   - the number is not open: closing something already gone.  The        *)
(*     kernel answers EBADF.  Mild on its own, and it is what made the     *)
(*     real bug look harmless in single-threaded tests.                    *)
(*   - the number is open but on a later generation: this close shuts a    *)
(*     door that now belongs to somebody else.  This is the one that       *)
(*     showed up as fattach failing with EBADF in an unrelated thread.     *)
(***************************************************************************)
Release(f, g) ==
    IF fdOpen[f] /\ fdGen[f] = g
        THEN /\ fdOpen' = [fdOpen EXCEPT ![f] = FALSE]
             /\ UNCHANGED <<stale, alien>>
    ELSE IF ~fdOpen[f]
        THEN /\ stale' = stale \cup {<<f, g>>}
             /\ UNCHANGED <<fdOpen, alien>>
    ELSE     /\ alien' = alien \cup {<<f, g>>}
             /\ UNCHANGED <<fdOpen, stale>>

\* door_create: the kernel hands back the lowest free number.
Open(t) ==
    /\ st[t] = "idle"
    /\ \E f \in FDs :
        /\ ~fdOpen[f]
        /\ fdGen[f] < MaxGen
        /\ fdOpen' = [fdOpen EXCEPT ![f] = TRUE]
        /\ fdGen' = [fdGen EXCEPT ![f] = fdGen[f] + 1]
        /\ myFd' = [myFd EXCEPT ![t] = f]
        /\ myGen' = [myGen EXCEPT ![t] = fdGen[f] + 1]
    /\ st' = [st EXCEPT ![t] = "opened"]
    /\ UNCHANGED <<lock, reg, forkDone, stale, alien>>

\* Joining the registry takes the lock.
Register(t) ==
    /\ st[t] = "opened"
    /\ lock = "free"
    /\ reg' = reg \cup {<<myFd[t], myGen[t]>>}
    /\ st' = [st EXCEPT ![t] = "serving"]
    /\ UNCHANGED <<lock, fdOpen, fdGen, myFd, myGen, forkDone, stale, alien>>

(***************************************************************************)
(* Teardown, step two: remove the entry, release the descriptor, drop the  *)
(* lock.  All of it inside the critical section, which is the rule.        *)
(*                                                                        *)
(* Under BUGGY the descriptor is released twice, which is what the crate   *)
(* did when it called door_revoke (which closes) and then close().        *)
(***************************************************************************)
(***************************************************************************)
(* The defect this crate shipped, modelled as it actually was.            *)
(*                                                                        *)
(* Door::drop called door_revoke OUTSIDE the registry lock, and           *)
(* door_revoke closes the descriptor.  Then deregister_and_close took the *)
(* lock and closed the same number a second time.  The gap between them   *)
(* holds no lock, so another thread can call door_create and be handed    *)
(* that very number -- and the second close then shuts ITS door.          *)
(*                                                                        *)
(* Only reachable when BUGGY is TRUE.                                     *)
(***************************************************************************)
TearRevoke(t) ==
    /\ BUGGY
    /\ st[t] = "serving"
    /\ Release(myFd[t], myGen[t])
    /\ st' = [st EXCEPT ![t] = "revoked"]
    /\ UNCHANGED <<lock, fdGen, reg, myFd, myGen, forkDone>>

\* Teardown, step two: take the lock.
TearLock(t) ==
    /\ IF BUGGY THEN st[t] = "revoked" ELSE st[t] = "serving"
    /\ lock = "free"
    /\ lock' = t
    /\ st' = [st EXCEPT ![t] = "tearing"]
    /\ UNCHANGED <<fdOpen, fdGen, reg, myFd, myGen, forkDone, stale, alien>>

\* Teardown, step three: remove the entry and release the descriptor,
\* both inside the critical section.  That is the rule GOALS.md 7.1
\* states.  Under BUGGY this is the SECOND release of the same number.
TearRelease(t) ==
    /\ st[t] = "tearing"
    /\ lock = t
    /\ reg' = reg \ {<<myFd[t], myGen[t]>>}
    /\ Release(myFd[t], myGen[t])
    /\ lock' = "free"
    /\ st' = [st EXCEPT ![t] = "idle"]
    /\ myFd' = [myFd EXCEPT ![t] = NONE]
    /\ UNCHANGED <<fdGen, myGen, forkDone>>

(***************************************************************************)
(* fork.  The prepare handler takes the lock in the parent, which is still *)
(* multithreaded.  The child inherits it already held, so it never has to  *)
(* acquire anything -- that is what makes the child handler legal, since   *)
(* it may only do async-signal-safe work.                                  *)
(*                                                                        *)
(* The child gets its own copy of the descriptor table, so its closes      *)
(* cannot touch the parent.  What the model checks here is weaker but      *)
(* still worth having: the child must never see an entry whose descriptor  *)
(* the parent had already released, because that is what "remove before    *)
(* release" is for.                                                        *)
(***************************************************************************)
ForkPrepare ==
    /\ ~forkDone
    /\ lock = "free"
    /\ lock' = "prepare"
    /\ UNCHANGED <<fdOpen, fdGen, reg, st, myFd, myGen, forkDone, stale, alien>>

ForkChild ==
    /\ lock = "prepare"
    /\ forkDone' = TRUE
    /\ lock' = "free"
    \* Every entry the child inherits must name a descriptor that is
    \* really open.  If not, the parent released before deregistering.
    /\ stale' = stale \cup
        {e \in reg : ~(fdOpen[e[1]] /\ fdGen[e[1]] = e[2])}
    /\ UNCHANGED <<fdOpen, fdGen, reg, st, myFd, myGen, alien>>

Next ==
    \/ \E t \in Threads : Open(t) \/ Register(t) \/ TearRevoke(t)
                          \/ TearLock(t) \/ TearRelease(t)
    \/ ForkPrepare
    \/ ForkChild

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* What must always hold.                                                  *)
(***************************************************************************)

\* Nobody ever closes a door that now belongs to somebody else.
NoAlienClose == alien = {}

\* Nobody ever closes a descriptor twice.
NoStaleClose == stale = {}

\* The registry never names a descriptor that is not open on that
\* generation.  This is "remove before release", stated directly.
RegistryHonest ==
    \A e \in reg : fdOpen[e[1]] /\ fdGen[e[1]] = e[2]

\* The child handler never acquires the lock; it inherits it held.
LockDiscipline == (lock = "prepare") => ~forkDone

TypeOK ==
    /\ fdOpen \in [FDs -> BOOLEAN]
    /\ reg \subseteq (FDs \X (0..MaxGen))
    /\ st \in [Threads -> {"idle", "opened", "serving", "revoked", "tearing"}]

=============================================================================
