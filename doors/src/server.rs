/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Copyright 2023 Robert D. French
 */
//! Traits for easier Server Procedures

use crate::illumos;
use crate::illumos::door_h::door_desc_t;
use crate::illumos::fattach;
use crate::illumos::DoorAttributes;
use crate::illumos::DoorFd;
use libc;
use std::ffi;
use std::fs::File;
use std::io;
use std::os::fd::RawFd;
use std::path::Path;

/// Door problems.
///
/// Two things can go wrong with a door -- its path can be invalid, or a system
/// call can fail. If a system call fails, one of this enum's variants will be
/// returned corresponding to the failed system call. It will contain the value
/// of `errno` associated with the failed system call.
#[derive(Debug)]
pub enum Error {
    InvalidPath(ffi::NulError),
    InstallJamb(std::io::Error),
    AttachDoor(illumos::Error),
    OpenDoor(std::io::Error),
    DoorCall(libc::c_int),
    CreateDoor(illumos::Error),
}

/// A Descriptor for the Door Server
///
/// When a door is created, the kernel hands us back a reference to it by giving
/// us an index in our descriptor table. This is true even if the door hasn't
/// been attached to the filesystem yet, a la pipes or sockets.
pub struct Door(RawFd);

impl Door {
    /// Create a new Door with the specified server procedure.  This will not
    /// expose the door to the filesystem by default. It will assume that you
    /// are not using a door cookie, and that you do not need to set any
    /// [`DoorAttributes`].
    pub fn create(sp: illumos::ServerProcedure) -> Result<Self, Error> {
        let cookie = 0;
        let attrs = DoorAttributes::none();
        Self::create_with_cookie_and_attributes(sp, cookie, attrs)
    }

    /// Create a new Door with a Cookie.  This will not expose the door to the
    /// filesystem by default. It will use the door cookie that you provide, but
    /// will assume that you do not need to set any [`DoorAttributes`].
    pub fn create_with_cookie(
        sp: illumos::ServerProcedure,
        cookie: u64,
    ) -> Result<Self, Error> {
        let attrs = DoorAttributes::none();
        Self::create_with_cookie_and_attributes(sp, cookie, attrs)
    }

    /// Create a new Door with Attributes.  This will not expose the door to the
    /// filesystem by default. It will use the [`DoorAttributes`] that you
    /// provide, but will assume that you are not using a door cookie.
    pub fn create_with_attributes(
        sp: illumos::ServerProcedure,
        attrs: DoorAttributes,
    ) -> Result<Self, Error> {
        let cookie = 0;
        Self::create_with_cookie_and_attributes(sp, cookie, attrs)
    }

    /// Create a new Door with Cookie and Attributes.  This will not expose the
    /// door to the filesystem by default. It will use the [`DoorAttributes`]
    /// and cookie that you provide.
    pub fn create_with_cookie_and_attributes(
        sp: illumos::ServerProcedure,
        cookie: u64,
        attrs: illumos::DoorAttributes,
    ) -> Result<Self, Error> {
        match illumos::door_create(sp, cookie, attrs) {
            Ok(fd) => Ok(Self(fd as RawFd)),
            Err(e) => Err(Error::CreateDoor(e)),
        }
    }

    /// Make this door server available on the filesystem.  This is necessary if
    /// we want other processes to be able to find and call this door server.
    pub fn install<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        // Create jamb
        let _jamb = match create_new_file(&path) {
            Ok(file) => file,
            Err(e) => return Err(Error::InstallJamb(e)),
        };

        // Attach door to jamb
        match fattach(self.0, &path) {
            Err(e) => {
                // Clean up the jamb, since we aren't going to finish
                std::fs::remove_file(&path).ok();
                Err(Error::AttachDoor(e))
            }
            Ok(()) => Ok(()),
        }
    }

    /// Make this door available on the filesystem even if there is already a
    /// file (possibly leftover from a previous door) as this path.
    pub fn force_install<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        if path.as_ref().exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                return Err(Error::InstallJamb(e));
            }
        }
        self.install(path)
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        unsafe {
            illumos::door_h::door_revoke(self.0);
        }
    }
}

/// Server-Side representation of the client's door arguments
///
/// This type allows us to write server procedures that accept a single argument
/// rather than five separate arguments.
#[derive(Copy, Clone)]
pub struct Request<'a> {
    pub cookie: u64,
    pub data: &'a [u8],
    pub descriptors: &'a [door_desc_t],
}

/// Server-Side representation of the client's door results
///
/// This type can refer to either memory on the stack (which will be cleaned up
/// automatically when door_return is called) or memory on the heap (which
/// will not). If you return an object that refers to memory on the heap, it is
/// your responsibility to free it later.
///
/// Many door servers allocate a per-thread response area so that each thread
/// can re-use this area for every door invocation assigned to it. That way the
/// memory leaked is constant. Typically, applications that take this approach
/// will free these per-thread response areas when the DOOR_UNREF message is
/// sent.
pub struct Response<C: AsRef<[u8]>> {
    pub data: Option<C>,
    pub num_descriptors: u32,
    pub descriptors: [DoorFd; 2],
}

impl<C: AsRef<[u8]>> Response<C> {
    pub fn new(data: C) -> Self {
        let descriptors = [DoorFd::new(-1, true), DoorFd::new(-1, true)];
        let num_descriptors = 0;
        Self {
            data: Some(data),
            descriptors,
            num_descriptors,
        }
    }

    pub fn empty() -> Self {
        let data = None;
        let descriptors = [DoorFd::new(-1, true), DoorFd::new(-1, true)];
        let num_descriptors = 0;
        Self {
            data,
            descriptors,
            num_descriptors,
        }
    }

    pub fn add_descriptor(mut self, fd: RawFd, release: bool) -> Self {
        if self.num_descriptors == 2 {
            panic!("Only 2 descriptors are supported")
        }

        let desc = DoorFd::new(fd, release);
        self.descriptors[self.num_descriptors as usize] = desc;
        self.num_descriptors += 1;

        self
    }
}

fn create_new_file<P: AsRef<Path>>(path: P) -> io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn create_new_fails_if_file_exists() {
        match File::create("/tmp/create_new_fail.txt") {
            // If we can't create the "original" file, we want the test to fail,
            // which means that we *don't* want to panic.
            Err(e) => {
                eprintln!("{:?}", e);
                assert!(true)
            }
            Ok(_file) => {
                create_new_file("/tmp/create_new_fail.txt").unwrap();
            }
        }
    }
}
