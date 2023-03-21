pub mod illumos;

use illumos::door_h;
use std::os::fd;
use std::os::fd::AsRawFd;

impl AsRawFd for door_h::door_desc_t {
    fn as_raw_fd(&self) -> fd::RawFd {
        let d_data = &self.d_data;
        let d_desc = unsafe { d_data.d_desc };
        let d_descriptor = d_desc.d_descriptor;
        d_descriptor as fd::RawFd
    }
}

impl door_h::door_desc_t {
    pub fn new(raw: fd::RawFd, release: bool) -> Self {
        let d_descriptor = raw as libc::c_int;
        let d_id = 0;
        let d_desc = door_h::door_desc_t__d_data__d_desc { d_descriptor, d_id };
        let d_data = door_h::door_desc_t__d_data { d_desc };

        let d_attributes = match release {
            false => door_h::DOOR_DESCRIPTOR,
            true => door_h::DOOR_DESCRIPTOR | door_h::DOOR_RELEASE,
        };
        Self {
            d_attributes,
            d_data,
        }
    }

    pub fn will_release(&self) -> bool {
        self.d_attributes == (door_h::DOOR_DESCRIPTOR | door_h::DOOR_RELEASE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_raw_fd() {
        let dd = door_h::door_desc_t::new(-1, true);
        assert_eq!(dd.as_raw_fd(), -1);
    }

    #[test]
    fn release() {
        let dd = door_h::door_desc_t::new(-1, true);
        assert!(dd.will_release());
    }
}
