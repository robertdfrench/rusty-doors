// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! `GOALS.md` §9.1: prove the mistakes really are compile errors.
//!
//! A test that checks a *runtime* refusal would pass just as well if
//! the typestate did nothing. These check that the code does not
//! build at all, which is the actual claim the crate makes.
//!
//! Like the rest of the crate, these run on illumos.

#[test]
fn the_typestate_rejects_what_it_should() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
