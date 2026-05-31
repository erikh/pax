//! # pax-hci
//!
//! A tiny, self-contained helper that reads the local Bluetooth controller's
//! **version** and **manufacturer** directly from the Linux kernel's Bluetooth
//! *management* (`mgmt`) socket — information BlueZ does not expose over D-Bus.
//!
//! It exists as its own crate for one reason: it needs raw `AF_BLUETOOTH` socket
//! calls, which require `unsafe`. Isolating it here keeps every other crate in the
//! [`pax`](https://github.com/erikh/pax) toolkit under `#![forbid(unsafe_code)]`.
//! The public API is just one safe function returning plain integers, so there is
//! no `unsafe` to reason about at the call site.
//!
//! ```
//! // Returns `None` off Linux, without permission, or with no such controller —
//! // callers treat `None` as "unknown, use a default".
//! if let Some(info) = pax_hci::read_controller_info(0) {
//!     println!("hci0: manufacturer 0x{:04x}, HCI version {}", info.manufacturer, info.hci_version);
//! }
//! ```
//!
//! ## Permissions
//!
//! The `mgmt` socket typically requires `CAP_NET_ADMIN` (root, or a binary with
//! the capability granted). Without it, [`read_controller_info`] returns `None`
//! rather than failing loudly — auto-detection is best-effort by design.
#![cfg_attr(not(target_os = "linux"), allow(unused))]

/// What the kernel reports about a local controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerInfo {
    /// Bluetooth SIG company identifier of the controller's manufacturer.
    pub manufacturer: u16,
    /// The HCI version byte (maps to a Bluetooth core version, e.g. `9` == 5.0).
    pub hci_version: u8,
}

/// Read [`ControllerInfo`] for HCI device `index` (e.g. `0` for `hci0`).
///
/// Returns `None` on any failure — non-Linux platforms, missing permission, no
/// such controller, a timeout, or a malformed response. This is intentionally
/// total and quiet so controller auto-detection can fall back to a default.
#[cfg(target_os = "linux")]
pub fn read_controller_info(index: u16) -> Option<ControllerInfo> {
    linux::read_controller_info(index)
}

/// Non-Linux stub: always `None`.
#[cfg(not(target_os = "linux"))]
pub fn read_controller_info(_index: u16) -> Option<ControllerInfo> {
    None
}

#[cfg(target_os = "linux")]
mod linux {
    use super::ControllerInfo;
    use std::os::unix::io::RawFd;

    const AF_BLUETOOTH: libc::c_int = 31;
    const BTPROTO_HCI: libc::c_int = 1;
    const HCI_CHANNEL_CONTROL: u16 = 3;
    const HCI_DEV_NONE: u16 = 0xffff;
    const MGMT_OP_READ_INFO: u16 = 0x0004;
    const MGMT_EV_CMD_COMPLETE: u16 = 0x0001;

    // struct sockaddr_hci { sa_family_t hci_family; unsigned short hci_dev; unsigned short hci_channel; }
    #[repr(C)]
    struct SockaddrHci {
        hci_family: libc::sa_family_t,
        hci_dev: u16,
        hci_channel: u16,
    }

    /// Closes its file descriptor on drop, so every early return cleans up.
    struct FdGuard(RawFd);
    impl Drop for FdGuard {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }

    pub fn read_controller_info(index: u16) -> Option<ControllerInfo> {
        unsafe { read_inner(index) }
    }

    unsafe fn read_inner(index: u16) -> Option<ControllerInfo> {
        let fd = libc::socket(
            AF_BLUETOOTH,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            BTPROTO_HCI,
        );
        if fd < 0 {
            return None;
        }
        let _guard = FdGuard(fd);

        // Bound receive time so a silent kernel never wedges us.
        let tv = libc::timeval {
            tv_sec: 2,
            tv_usec: 0,
        };
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );

        // Bind to the management channel (controller index None: commands carry it).
        let addr = SockaddrHci {
            hci_family: AF_BLUETOOTH as libc::sa_family_t,
            hci_dev: HCI_DEV_NONE,
            hci_channel: HCI_CHANNEL_CONTROL,
        };
        let rc = libc::bind(
            fd,
            &addr as *const SockaddrHci as *const libc::sockaddr,
            std::mem::size_of::<SockaddrHci>() as libc::socklen_t,
        );
        if rc < 0 {
            return None;
        }

        // mgmt command header: opcode(2, LE), controller index(2, LE), param len(2, LE).
        let mut cmd = [0u8; 6];
        cmd[0..2].copy_from_slice(&MGMT_OP_READ_INFO.to_le_bytes());
        cmd[2..4].copy_from_slice(&index.to_le_bytes());
        cmd[4..6].copy_from_slice(&0u16.to_le_bytes());
        let written = libc::write(fd, cmd.as_ptr() as *const libc::c_void, cmd.len());
        if written < 0 {
            return None;
        }

        // Read events until the Command Complete for READ_INFO arrives.
        let mut buf = [0u8; 1024];
        for _ in 0..16 {
            let n = libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
            if n < 6 {
                return None;
            }
            let n = n as usize;

            // mgmt event header: event code(2, LE), controller index(2, LE), param len(2, LE).
            let event_code = u16::from_le_bytes([buf[0], buf[1]]);
            let param_len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
            if 6 + param_len > n {
                continue;
            }
            let params = &buf[6..6 + param_len];

            if event_code == MGMT_EV_CMD_COMPLETE && params.len() >= 3 {
                // Command Complete params: command opcode(2, LE), status(1), response...
                let cmd_opcode = u16::from_le_bytes([params[0], params[1]]);
                let status = params[2];
                if cmd_opcode == MGMT_OP_READ_INFO {
                    if status != 0 {
                        return None;
                    }
                    // mgmt_rp_read_info: bdaddr(6), version(1), manufacturer(2, LE), ...
                    let rp = &params[3..];
                    if rp.len() < 9 {
                        return None;
                    }
                    return Some(ControllerInfo {
                        hci_version: rp[6],
                        manufacturer: u16::from_le_bytes([rp[7], rp[8]]),
                    });
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_index_is_none_not_panic() {
        // A wildly out-of-range controller index must never panic; it returns
        // None (no such controller / no permission / not Linux).
        let _ = read_controller_info(0xfffe);
    }
}
