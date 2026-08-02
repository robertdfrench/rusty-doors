--------------------------- MODULE RevokeDrainOrder ---------------------------
(***************************************************************************)
(* The same model, run with a different configuration.                     *)
(*                                                                        *)
(* TLC reads the .cfg whose name matches the module, so each              *)
(* configuration needs a module of its own.  Everything comes from         *)
(* RevokeDrain; only the .cfg beside this file differs.  Keeping the       *)
(* model in one place is the point -- a copied model drifts, and then the  *)
(* clean runs and the failing runs stop describing the same thing.         *)
(***************************************************************************)
EXTENDS RevokeDrain
=============================================================================
