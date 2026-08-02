------------------------------ MODULE RevokeDrain ------------------------------
(***************************************************************************)
(* `Door::revoke()` and the drain loop.                                    *)
(*                                                                        *)
(* `GOALS.md` 5.1 says revoke must revoke, wait for the in-flight counter  *)
(* to reach zero, drop the state and return it.  Two facts are already     *)
(* known and neither is assumed here; both come out of the model:          *)
(*                                                                        *)
(*   - `in_flight` is never incremented.  Only the initialiser and the     *)
(*     load in `Door::revoke` exist.  The drain loop therefore reads zero  *)
(*     and returns at once, so revoke does not wait for anything.          *)
(*                                                                        *)
(*   - Even with the increment, there is a window between the kernel       *)
(*     entering the trampoline and the trampoline raising the counter.  A  *)
(*     revoke that reads the counter inside that window sees zero while a  *)
(*     call is about to resolve the cookie.  Whether that window is        *)
(*     harmful was an open question.  It is answered below.                *)
(*                                                                        *)
(* WHAT IS MODELLED                                                       *)
(*                                                                        *)
(* Server threads handling calls, one revoking thread, the counter, and    *)
(* the lifetime of the state.  The slab is here too, because the slab is   *)
(* what decides whether a late cookie resolves at all.                     *)
(*                                                                        *)
(* WHY A SMALL POOL AND A GENERATION                                      *)
(*                                                                        *)
(* There is exactly one slab slot, and it is reused.  That is on purpose.  *)
(* If every door had its own fresh slot, a stale cookie could never meet   *)
(* a new tenant, and the generation counter -- the thing that makes a      *)
(* stale cookie safe -- would never be tested.  So the slot is reused and  *)
(* carries a generation, and a cookie holding an old generation must       *)
(* resolve to nothing rather than to whoever moved in.                     *)
(*                                                                        *)
(* THE THREE KNOBS                                                        *)
(*                                                                        *)
(*   INCREMENT  where the counter is raised, relative to resolving the     *)
(*              cookie: "none" (the crate today), "after", or "before".    *)
(*                                                                        *)
(*   ARCCLONE   TRUE  -- resolving clones an `Arc` under the slab lock, so *)
(*                       an in-flight call holds its own reference.  This  *)
(*                       is the crate today.                               *)
(*              FALSE -- resolving hands back a bare pointer and holds no  *)
(*                       reference.  This was the old `Pinned` strategy.   *)
(*                       With it, the drain is the ONLY thing keeping the  *)
(*                       state alive, so it is the setting that says what  *)
(*                       the drain is worth on its own.                    *)
(*                                                                        *)
(*   ORDER      "drainThenUninstall" -- wait for the counter, then take    *)
(*                       the state out of the slab.  This is what          *)
(*                       `Door::revoke` does now.                          *)
(*              "uninstallThenDrain" -- take it out of the slab first,     *)
(*                       then wait.                                        *)
(*                                                                        *)
(* A model that has never failed is worth nothing, so several of the       *)
(* configurations beside this file MUST fail.  See specs/README.md for     *)
(* which, and for the traces.                                             *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS
    Threads,    \* the server threads, deliberately few
    NumStates,  \* how many doors may be built in turn, deliberately few
    INCREMENT,  \* "none", "after" or "before"
    ARCCLONE,   \* does resolving take a reference of its own
    ORDER       \* "drainThenUninstall" or "uninstallThenDrain"

\* The states, one per door built.  State 1 belongs to the first door,
\* state 2 to the door that takes the slot after it, and so on.
States == 1..NumStates

\* "no state" -- an empty slot, a thread holding nothing.
NONE == 0

VARIABLES
    slotGen,    \* the generation stamped on the one slab slot
    slotState,  \* the state installed in that slot, or NONE
    taken,      \* the state revoke has pulled out and not finished with
    built,      \* how many doors have been built so far
    freed,      \* state -> has its allocation been released
    refs,       \* state -> how many in-flight calls hold a clone
    inFlight,   \* the AtomicUsize
    revoked,    \* has door_revoke run for the door now installed
    rv,         \* where the revoking thread is
    shared,     \* did Arc::try_unwrap find the state still shared
    ph,         \* thread -> where it is inside the trampoline
    ck,         \* thread -> the generation its cookie carries
    held,       \* thread -> the state it resolved, or NONE
    born,       \* thread -> the state installed when the kernel dispatched
    inc         \* thread -> has this thread raised the counter

vars ==
    <<slotGen, slotState, taken, built, freed, refs, inFlight, revoked,
      rv, shared, ph, ck, held, born, inc>>

(***************************************************************************)
(* Where a server thread can be.                                          *)
(*                                                                        *)
(* "entered" is the window the open question is about: the kernel is       *)
(* inside the trampoline and nothing has happened yet.                     *)
(***************************************************************************)
Phases ==
    { "idle",
      "entered",    \* the kernel dispatched a call here; nothing done yet
      "counted",    \* INCREMENT = "before": counter up, cookie not resolved
      "resolved",   \* INCREMENT = "after": cookie resolved, counter not up
      "running",    \* inside the user function, holding the state
      "faulted",    \* the cookie did not resolve; holding nothing
      "dropped" }   \* end of scope 1: the state is let go, counter still up

RvPhases == {"idle", "drain", "uninstall", "unwrap", "done"}

MaxThreads == Cardinality(Threads)

Init ==
    /\ slotGen   = 1
    /\ slotState = 1              \* the first door is already built
    /\ taken     = NONE
    /\ built     = 1
    /\ freed     = [s \in States |-> FALSE]
    /\ refs      = [s \in States |-> 0]
    /\ inFlight  = 0
    /\ revoked   = FALSE
    /\ rv        = "idle"
    /\ shared    = FALSE
    /\ ph        = [t \in Threads |-> "idle"]
    /\ ck        = [t \in Threads |-> 0]
    /\ held      = [t \in Threads |-> NONE]
    /\ born      = [t \in Threads |-> NONE]
    /\ inc       = [t \in Threads |-> FALSE]

(***************************************************************************)
(* The kernel dispatches a call to a server thread.                       *)
(*                                                                        *)
(* This is the top of the trampoline.  The cookie was baked into the door  *)
(* when it was created, so the thread carries whatever generation was      *)
(* current then.  After door_revoke the kernel starts no new calls, but a  *)
(* thread already standing here is not called back.                        *)
(***************************************************************************)
Dispatch(t) ==
    /\ ph[t] = "idle"
    /\ ~revoked
    /\ slotState # NONE
    /\ ph'   = [ph   EXCEPT ![t] = "entered"]
    /\ ck'   = [ck   EXCEPT ![t] = slotGen]
    /\ born' = [born EXCEPT ![t] = slotState]
    /\ UNCHANGED <<slotGen, slotState, taken, built, freed, refs, inFlight,
                   revoked, rv, shared, held, inc>>

\* INCREMENT = "before": raise the counter first, then look the cookie up.
RaiseBefore(t) ==
    /\ INCREMENT = "before"
    /\ ph[t] = "entered"
    /\ inFlight' = inFlight + 1
    /\ inc' = [inc EXCEPT ![t] = TRUE]
    /\ ph'  = [ph  EXCEPT ![t] = "counted"]
    /\ UNCHANGED <<slotGen, slotState, taken, built, freed, refs, revoked,
                   rv, shared, ck, held, born>>

(***************************************************************************)
(* Resolving the cookie.                                                  *)
(*                                                                        *)
(* This is a lookup in the slab, not a pointer read.  Three things can     *)
(* happen and only one of them hands the thread a state:                   *)
(*                                                                        *)
(*   - the slot is empty, because revoke took the state out.  No state.    *)
(*   - the slot holds somebody else, and the generation says so.  No       *)
(*     state.  This is the stale cookie the generation exists for.         *)
(*   - the generation matches.  The thread gets the state.                 *)
(*                                                                        *)
(* Under ARCCLONE the match also clones an `Arc`, in the same step, under  *)
(* the same lock.  "Is it still there?" and "take a reference" cannot come *)
(* apart, which is what makes the state outlive the call.                  *)
(***************************************************************************)
Resolve(t) ==
    /\ ph[t] = (IF INCREMENT = "before" THEN "counted" ELSE "entered")
    /\ IF slotState # NONE /\ slotGen = ck[t]
         THEN /\ held' = [held EXCEPT ![t] = slotState]
              /\ refs' = IF ARCCLONE
                           THEN [refs EXCEPT ![slotState] = @ + 1]
                           ELSE refs
              /\ ph'   = [ph EXCEPT ![t] =
                            IF INCREMENT = "after" THEN "resolved"
                                                   ELSE "running"]
         ELSE /\ ph' = [ph EXCEPT ![t] = "faulted"]
              /\ UNCHANGED <<held, refs>>
    /\ UNCHANGED <<slotGen, slotState, taken, built, freed, inFlight,
                   revoked, rv, shared, ck, born, inc>>

\* INCREMENT = "after": raise the counter once the state is in hand.
\* A resolve that failed never gets here, because the real code replies
\* with a fault and returns.
RaiseAfter(t) ==
    /\ INCREMENT = "after"
    /\ ph[t] = "resolved"
    /\ inFlight' = inFlight + 1
    /\ inc' = [inc EXCEPT ![t] = TRUE]
    /\ ph'  = [ph  EXCEPT ![t] = "running"]
    /\ UNCHANGED <<slotGen, slotState, taken, built, freed, refs, revoked,
                   rv, shared, ck, held, born>>

(***************************************************************************)
(* The end of scope 1: the user function has returned and the thread lets  *)
(* the state go.                                                          *)
(*                                                                        *)
(* Letting go and lowering the counter are two separate steps here, in     *)
(* this order, on purpose.  If the counter went down first, a drain could  *)
(* read zero while a clone was still alive.  The order is part of what is  *)
(* being checked, not a detail of the model.                               *)
(*                                                                        *)
(* Under ARCCLONE, dropping the last clone is what frees the allocation --  *)
(* but only if the slab has given the state up and revoke has finished     *)
(* with it too.                                                            *)
(***************************************************************************)
LetGo(t) ==
    /\ ph[t] \in {"running", "faulted"}
    /\ IF ARCCLONE /\ held[t] # NONE
         THEN /\ refs' = [refs EXCEPT ![held[t]] = @ - 1]
              /\ freed' = IF /\ refs[held[t]] - 1 = 0
                             /\ slotState # held[t]
                             /\ taken # held[t]
                            THEN [freed EXCEPT ![held[t]] = TRUE]
                            ELSE freed
         ELSE UNCHANGED <<refs, freed>>
    /\ held' = [held EXCEPT ![t] = NONE]
    /\ ph'   = [ph   EXCEPT ![t] = "dropped"]
    /\ UNCHANGED <<slotGen, slotState, taken, built, inFlight, revoked,
                   rv, shared, ck, born, inc>>

\* door_return.  The counter comes down, if this thread ever put it up.
Return(t) ==
    /\ ph[t] = "dropped"
    /\ inFlight' = IF inc[t] THEN inFlight - 1 ELSE inFlight
    /\ inc'  = [inc  EXCEPT ![t] = FALSE]
    /\ ph'   = [ph   EXCEPT ![t] = "idle"]
    /\ ck'   = [ck   EXCEPT ![t] = 0]
    /\ born' = [born EXCEPT ![t] = NONE]
    /\ UNCHANGED <<slotGen, slotState, taken, built, freed, refs, revoked,
                   rv, shared, held>>

(***************************************************************************)
(* `Door::revoke()`, in four steps.                                       *)
(*                                                                        *)
(* Step one is always door_revoke: after it the kernel starts no new       *)
(* calls.  The middle two are the drain and the slab removal, and ORDER    *)
(* decides which comes first.  Step four is `Arc::try_unwrap`.            *)
(***************************************************************************)
RvRevoke ==
    /\ rv = "idle"
    /\ slotState # NONE
    /\ revoked' = TRUE
    /\ rv' = IF ORDER = "drainThenUninstall" THEN "drain" ELSE "uninstall"
    /\ UNCHANGED <<slotGen, slotState, taken, built, freed, refs, inFlight,
                   shared, ph, ck, held, born, inc>>

\* The drain loop.  It spins while the counter is above zero, so there is
\* simply no step to take until it reaches zero.
RvDrain ==
    /\ rv = "drain"
    /\ inFlight = 0
    /\ rv' = IF ORDER = "drainThenUninstall" THEN "uninstall" ELSE "unwrap"
    /\ UNCHANGED <<slotGen, slotState, taken, built, freed, refs, inFlight,
                   revoked, shared, ph, ck, held, born, inc>>

\* Take the state out of the slab and move the slot's generation on, in
\* one step, under the one lock.  Every cookie for the old door is dead
\* from here.
RvUninstall ==
    /\ rv = "uninstall"
    /\ taken' = slotState
    /\ slotState' = NONE
    /\ slotGen' = slotGen + 1
    /\ rv' = IF ORDER = "drainThenUninstall" THEN "unwrap" ELSE "drain"
    /\ UNCHANGED <<built, freed, refs, inFlight, revoked, shared,
                   ph, ck, held, born, inc>>

(***************************************************************************)
(* `Arc::try_unwrap`.                                                     *)
(*                                                                        *)
(* Under ARCCLONE, if an in-flight call still holds a clone then revoke    *)
(* cannot hand back an owned S and returns `Err(StateStillShared)`.  The   *)
(* allocation is not freed here; the last call to let go frees it.         *)
(*                                                                        *)
(* Without ARCCLONE there is no reference count at all.  Revoke is the     *)
(* only owner by construction, so it frees the state right here -- even    *)
(* if a call is standing in the middle of using it.  That is the whole     *)
(* hazard this model exists to look for.                                   *)
(***************************************************************************)
RvUnwrap ==
    /\ rv = "unwrap"
    /\ IF ARCCLONE /\ refs[taken] > 0
         THEN /\ shared' = TRUE
              /\ UNCHANGED freed
         ELSE /\ freed' = [freed EXCEPT ![taken] = TRUE]
              /\ UNCHANGED shared
    /\ taken' = NONE
    /\ rv' = "done"
    /\ UNCHANGED <<slotGen, slotState, built, refs, inFlight, revoked,
                   ph, ck, held, born, inc>>

(***************************************************************************)
(* Another door is built and takes the same slab slot.                    *)
(*                                                                        *)
(* This is what makes the generation stamp matter.  A thread still         *)
(* carrying the old door's cookie now points at a slot that holds          *)
(* somebody else's state.                                                 *)
(***************************************************************************)
Rebuild ==
    /\ rv = "done"
    /\ slotState = NONE
    /\ built < NumStates
    /\ built' = built + 1
    /\ slotState' = built + 1
    /\ revoked' = FALSE
    /\ rv' = "idle"
    /\ UNCHANGED <<slotGen, taken, freed, refs, inFlight, shared,
                   ph, ck, held, born, inc>>

Next ==
    \/ \E t \in Threads :
        \/ Dispatch(t) \/ RaiseBefore(t) \/ Resolve(t) \/ RaiseAfter(t)
        \/ LetGo(t) \/ Return(t)
    \/ RvRevoke \/ RvDrain \/ RvUninstall \/ RvUnwrap \/ Rebuild

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* What must always hold.                                                 *)
(***************************************************************************)

(***************************************************************************)
(* The state is never freed while a call is holding it.                   *)
(*                                                                        *)
(* `held` is set the moment the cookie resolves and cleared at the end of  *)
(* scope 1, so this says exactly "between resolving the cookie and         *)
(* returning".  This is the memory-safety question.                       *)
(***************************************************************************)
NoUseAfterFree ==
    \A t \in Threads : (held[t] # NONE) => ~freed[held[t]]

(***************************************************************************)
(* `revoke()` hands back an owned value, not `Err(StateStillShared)`.     *)
(*                                                                        *)
(* This is not memory safety.  It is the shape of the result `GOALS.md`    *)
(* 5.1 promises, and it is the thing the drain is actually for once the    *)
(* slab holds a reference of its own.                                     *)
(***************************************************************************)
RevokeGetsOwned == ~shared

(***************************************************************************)
(* A call never runs against a state other than the one that was          *)
(* installed when the kernel dispatched to it.                            *)
(*                                                                        *)
(* This is the generation stamp, stated as a rule.  Without it, a cookie   *)
(* from a revoked door would resolve to whichever door has since taken     *)
(* the slot.                                                              *)
(***************************************************************************)
NoCrossTenant ==
    \A t \in Threads : (held[t] # NONE) => held[t] = born[t]

TypeOK ==
    /\ slotGen   \in 1..(NumStates + 1)
    /\ slotState \in States \cup {NONE}
    /\ taken     \in States \cup {NONE}
    /\ built     \in 1..NumStates
    /\ freed     \in [States -> BOOLEAN]
    /\ refs      \in [States -> 0..MaxThreads]
    /\ inFlight  \in 0..MaxThreads
    /\ rv        \in RvPhases
    /\ ph        \in [Threads -> Phases]
    /\ held      \in [Threads -> States \cup {NONE}]
    /\ born      \in [Threads -> States \cup {NONE}]

=============================================================================
