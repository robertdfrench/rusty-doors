----------------------- MODULE ReleasedExactlyOnceAlien -----------------------
(***************************************************************************)
(* The same model again, with the defect put back, but asking only about   *)
(* the worst outcome: a close that lands on somebody else's door.          *)
(*                                                                        *)
(* Checking only NoAlienRelease makes TLC walk past the milder symptoms    *)
(* -- the close of a descriptor the kernel already owns, and the close of  *)
(* a number that is simply gone -- and keep going until the number has     *)
(* been handed to another thread.  That is the trace worth reading.        *)
(***************************************************************************)
EXTENDS ReleasedExactlyOnce
=============================================================================
