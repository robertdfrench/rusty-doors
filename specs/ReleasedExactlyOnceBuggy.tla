----------------------- MODULE ReleasedExactlyOnceBuggy -----------------------
(***************************************************************************)
(* The same model, run with the defect put back.                          *)
(*                                                                        *)
(* TLC takes the configuration from the file whose name matches the       *)
(* module, so a second configuration needs a second module.  There is      *)
(* nothing new here: everything comes from ReleasedExactlyOnce, and only   *)
(* the .cfg beside this file differs.  Keeping the model in one place is   *)
(* the point -- a copied model drifts, and then the clean run and the      *)
(* failing run stop describing the same thing.                             *)
(***************************************************************************)
EXTENDS ReleasedExactlyOnce
=============================================================================
