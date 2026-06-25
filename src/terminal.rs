#[cfg(not(coverage))]
use nix::{ioctl_read_bad, ioctl_write_int_bad};
#[cfg(not(coverage))]
use std::fs::{File, OpenOptions};
#[cfg(not(coverage))]
use std::os::unix::io::AsRawFd;

#[cfg(not(coverage))]
use crate::error::Error;

#[cfg(not(coverage))]
const VT_GETSTATE: libc::c_ulong = 0x5603;
#[cfg(not(coverage))]
const VT_OPENQRY: libc::c_ulong = 0x5600;
#[cfg(not(coverage))]
const VT_ACTIVATE: libc::c_ulong = 0x5606;
#[cfg(not(coverage))]
const KDSETMODE: libc::c_ulong = 0x4B3A;

#[cfg(not(coverage))]
const KD_TEXT: i32 = 0x00;

#[cfg(not(coverage))]
#[repr(C)]
#[derive(Default)]
struct VtState {
    v_active: u16,
    v_signal: u16,
    v_state: u16,
}

#[cfg(not(coverage))]
ioctl_read_bad!(vt_getstate, VT_GETSTATE, VtState);
#[cfg(not(coverage))]
ioctl_read_bad!(vt_openqry, VT_OPENQRY, i32);
#[cfg(not(coverage))]
ioctl_write_int_bad!(vt_activate, VT_ACTIVATE);
#[cfg(not(coverage))]
ioctl_write_int_bad!(kd_setmode, KDSETMODE);

#[cfg(not(coverage))]
pub struct Terminal {
    file: File,
}

#[cfg(not(coverage))]
impl Terminal {
    pub fn open(path: &str) -> Result<Self, Error> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Terminal { file })
    }

    pub fn current_vt(&self) -> Result<u32, Error> {
        let mut state = VtState::default();
        unsafe { vt_getstate(self.file.as_raw_fd(), &mut state)? };
        Ok(state.v_active as u32)
    }

    pub fn next_vt(&self) -> Result<u32, Error> {
        let mut vt: i32 = 0;
        unsafe { vt_openqry(self.file.as_raw_fd(), &mut vt)? };
        Ok(vt as u32)
    }

    pub fn activate(&self, vt: u32) -> Result<(), Error> {
        unsafe { vt_activate(self.file.as_raw_fd(), vt as i32)? };
        Ok(())
    }

    pub fn set_text_mode(&self) -> Result<(), Error> {
        unsafe { kd_setmode(self.file.as_raw_fd(), KD_TEXT)? };
        Ok(())
    }
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TerminalMode {
    Vt { path: String, vt: u32, switch: bool },
    Stdin,
}
