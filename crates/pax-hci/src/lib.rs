//! # pax-hci
//!
//! A tiny, self-contained helper that talks to the Linux kernel's Bluetooth
//! *management* (`mgmt`) socket directly — for things BlueZ does not expose over
//! D-Bus. Today that is two jobs:
//!
//! * **Read** the local controller's **version** and **manufacturer**
//!   ([`read_controller_info`]).
//! * **Set** the controller's **public Bluetooth address**
//!   ([`set_public_address`] / [`spoof_public_address`]) — i.e. make the adapter
//!   present a different `BD_ADDR` on the air.
//!
//! It exists as its own crate for one reason: it needs raw `AF_BLUETOOTH` socket
//! calls, which require `unsafe`. Isolating it here keeps every other crate in the
//! [`pax`](https://github.com/erikh/pax) toolkit under `#![forbid(unsafe_code)]`.
//!
//! ```
//! // Returns `None` off Linux, without permission, or with no such controller —
//! // callers treat `None` as "unknown, use a default".
//! if let Some(info) = pax_hci::read_controller_info(0) {
//!     println!("hci0: manufacturer 0x{:04x}, HCI version {}", info.manufacturer, info.hci_version);
//! }
//! ```
//!
//! ## Impersonating an address
//!
//! [`spoof_public_address`] makes the **local** adapter advertise an arbitrary
//! public `BD_ADDR`. This is the same operation `btmgmt public-addr` performs: it
//! powers the controller off, stages the new address, and powers it back on.
//!
//! ```no_run
//! use pax_hci::spoof_public_address;
//!
//! // Take on the address AA:BB:CC:DD:EE:FF on hci0. Octets are in display order
//! // (most-significant first), the same order as `pax_core::BdAddr::octets()`.
//! match spoof_public_address(0, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]) {
//!     Ok(()) => println!("hci0 now presents AA:BB:CC:DD:EE:FF"),
//!     Err(e) => eprintln!("could not change address: {e}"),
//! }
//! ```
//!
//! This only rewrites your *own* controller's identity — useful for privacy,
//! testing a peer's pairing/allow-list logic, device migration, and authorized
//! security research. It cannot affect any remote device.
//!
//! **Caveats.** Not every controller can change its address: the kernel driver
//! must support it (many Broadcom/CSR/Intel parts do, others return *Not
//! Supported*). The change does **not** persist across a hardware reset/reboot —
//! re-apply it as needed. BlueZ (`bluetoothd`) may also re-read or override the
//! address when it next manages the adapter.
//!
//! ## Permissions
//!
//! The `mgmt` socket requires `CAP_NET_ADMIN` (root, or a binary granted the
//! capability). [`read_controller_info`] returns `None` without it; the
//! address-setting functions return [`SetAddressError::Command`] with a
//! *Permission Denied* status (or an [`SetAddressError::Io`] error binding the
//! socket).
#![cfg_attr(not(target_os = "linux"), allow(unused))]
#![warn(missing_docs)]

use std::fmt;

/// What the kernel reports about a local controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerInfo {
    /// Bluetooth SIG company identifier of the controller's manufacturer.
    pub manufacturer: u16,
    /// The HCI version byte (maps to a Bluetooth core version, e.g. `9` == 5.0).
    pub hci_version: u8,
}

/// Why staging or applying a new controller address failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum SetAddressError {
    /// The `mgmt` socket is Linux-only; this platform has no such interface.
    Unsupported,
    /// An OS-level failure opening, binding, writing to, or reading from the
    /// `mgmt` control socket — including a timeout waiting for the kernel reply.
    Io(std::io::Error),
    /// The kernel rejected the command with a Bluetooth *management* status code.
    /// The most likely values here are:
    ///
    /// * `0x0B` **Rejected** — usually the adapter was still powered on.
    /// * `0x0C` **Not Supported** — the controller cannot change its address.
    /// * `0x11` **Invalid Index** — no controller at that index.
    /// * `0x14` **Permission Denied** — missing `CAP_NET_ADMIN`.
    ///
    /// Use [`SetAddressError::status_str`] for a human-readable label.
    Command(u8),
}

impl SetAddressError {
    /// A human-readable label for a Bluetooth *management* status byte (the value
    /// carried by [`SetAddressError::Command`]).
    pub fn status_str(status: u8) -> &'static str {
        match status {
            0x00 => "success",
            0x01 => "unknown command",
            0x02 => "not connected",
            0x03 => "failed",
            0x04 => "connect failed",
            0x05 => "authentication failed",
            0x06 => "not paired",
            0x07 => "no resources",
            0x08 => "timeout",
            0x09 => "already connected",
            0x0A => "busy",
            0x0B => "rejected (is the adapter still powered on?)",
            0x0C => "not supported (controller cannot change its address)",
            0x0D => "invalid parameters",
            0x0E => "disconnected",
            0x0F => "not powered",
            0x10 => "cancelled",
            0x11 => "invalid index (no such controller)",
            0x12 => "rf-killed",
            0x13 => "already paired",
            0x14 => "permission denied (need CAP_NET_ADMIN)",
            _ => "unknown status",
        }
    }
}

impl fmt::Display for SetAddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetAddressError::Unsupported => {
                f.write_str("setting the controller address is only supported on Linux")
            }
            SetAddressError::Io(e) => write!(f, "mgmt socket I/O error: {e}"),
            SetAddressError::Command(s) => {
                write!(f, "mgmt command failed: {} (0x{s:02x})", Self::status_str(*s))
            }
        }
    }
}

impl std::error::Error for SetAddressError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SetAddressError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SetAddressError {
    fn from(e: std::io::Error) -> Self {
        SetAddressError::Io(e)
    }
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

/// Stage a new **public** `BD_ADDR` for controller `index`.
///
/// `addr` is in transmission/display order (most-significant octet first) — the
/// same order as `pax_core::BdAddr::octets()` and the text form
/// `AA:BB:CC:DD:EE:FF`. This crate reverses it to the little-endian wire layout
/// the kernel expects.
///
/// The controller **must be powered off** for this to be accepted, and the new
/// address only takes effect on the next power-on. Most callers want
/// [`spoof_public_address`], which performs the full power-cycle. Returns
/// [`SetAddressError::Command`] with status `0x0C` if the controller's driver
/// does not support changing its address.
#[cfg(target_os = "linux")]
pub fn set_public_address(index: u16, addr: [u8; 6]) -> Result<(), SetAddressError> {
    linux::set_public_address(index, addr)
}

/// Non-Linux stub: always [`SetAddressError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub fn set_public_address(_index: u16, _addr: [u8; 6]) -> Result<(), SetAddressError> {
    Err(SetAddressError::Unsupported)
}

/// Power controller `index` on (`true`) or off (`false`) via the `mgmt` API.
///
/// Setting an address requires the controller to be powered off first; this is
/// the primitive [`spoof_public_address`] uses to bracket the change.
#[cfg(target_os = "linux")]
pub fn set_powered(index: u16, on: bool) -> Result<(), SetAddressError> {
    linux::set_powered(index, on)
}

/// Non-Linux stub: always [`SetAddressError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub fn set_powered(_index: u16, _on: bool) -> Result<(), SetAddressError> {
    Err(SetAddressError::Unsupported)
}

/// Impersonate `addr` on controller `index`: power it **off**, set its public
/// address, then power it back **on** — the same sequence as
/// `btmgmt power off; btmgmt public-addr …; btmgmt power on`.
///
/// `addr` is in display order (most-significant octet first), matching
/// `pax_core::BdAddr::octets()`. On success the adapter presents `addr` as its
/// `BD_ADDR` until the next hardware reset or until something else (e.g. BlueZ)
/// changes it.
///
/// This affects only the **local** controller. See the [crate docs](crate) for
/// the controller-support and persistence caveats.
///
/// ```no_run
/// # use pax_hci::spoof_public_address;
/// spoof_public_address(0, [0x02, 0x00, 0x00, 0x12, 0x34, 0x56]).unwrap();
/// ```
#[cfg(target_os = "linux")]
pub fn spoof_public_address(index: u16, addr: [u8; 6]) -> Result<(), SetAddressError> {
    // Power off (a no-op if it was already off), stage the address, power on.
    set_powered(index, false)?;
    set_public_address(index, addr)?;
    set_powered(index, true)
}

/// Non-Linux stub: always [`SetAddressError::Unsupported`].
#[cfg(not(target_os = "linux"))]
pub fn spoof_public_address(_index: u16, _addr: [u8; 6]) -> Result<(), SetAddressError> {
    Err(SetAddressError::Unsupported)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{ControllerInfo, SetAddressError};
    use std::io;
    use std::os::unix::io::RawFd;

    const AF_BLUETOOTH: libc::c_int = 31;
    const BTPROTO_HCI: libc::c_int = 1;
    const HCI_CHANNEL_CONTROL: u16 = 3;
    const HCI_DEV_NONE: u16 = 0xffff;
    const MGMT_OP_READ_INFO: u16 = 0x0004;
    const MGMT_OP_SET_POWERED: u16 = 0x0005;
    // Per the kernel's `include/net/bluetooth/mgmt.h`. NB: this is 0x0039, *not*
    // 0x0050 (which is a different, parameterless command — sending it a 6-byte
    // address gets rejected with INVALID_PARAMS, 0x0d).
    const MGMT_OP_SET_PUBLIC_ADDRESS: u16 = 0x0039;
    const MGMT_EV_CMD_COMPLETE: u16 = 0x0001;
    const MGMT_EV_CMD_STATUS: u16 = 0x0002;

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

    /// Open and bind the `mgmt` control socket with a receive timeout (so a
    /// silent kernel never wedges us). The bound fd carries no controller index;
    /// each command names its own.
    unsafe fn open_control(timeout_secs: i64) -> io::Result<FdGuard> {
        let fd = libc::socket(
            AF_BLUETOOTH,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            BTPROTO_HCI,
        );
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let guard = FdGuard(fd);

        let tv = libc::timeval {
            tv_sec: timeout_secs,
            tv_usec: 0,
        };
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const libc::timeval as *const libc::c_void,
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        );

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
            return Err(io::Error::last_os_error());
        }
        Ok(guard)
    }

    /// Build the on-the-wire bytes of one `mgmt` command: a 6-byte header
    /// (opcode, controller index, param length — each little-endian) followed by
    /// the parameters. Pure, so the framing can be unit-tested without a socket.
    fn encode_command(opcode: u16, index: u16, params: &[u8]) -> Vec<u8> {
        let mut cmd = Vec::with_capacity(6 + params.len());
        cmd.extend_from_slice(&opcode.to_le_bytes());
        cmd.extend_from_slice(&index.to_le_bytes());
        cmd.extend_from_slice(&(params.len() as u16).to_le_bytes());
        cmd.extend_from_slice(params);
        cmd
    }

    /// A display-order (MSB-first) address as the little-endian (LSB-first) bytes
    /// the mgmt `bdaddr_t` wire format expects.
    fn bdaddr_le(addr: [u8; 6]) -> [u8; 6] {
        let mut le = addr;
        le.reverse();
        le
    }

    /// Write one `mgmt` command (header + params) to the bound socket.
    unsafe fn write_command(fd: RawFd, opcode: u16, index: u16, params: &[u8]) -> io::Result<()> {
        let cmd = encode_command(opcode, index, params);
        let written = libc::write(fd, cmd.as_ptr() as *const libc::c_void, cmd.len());
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn read_controller_info(index: u16) -> Option<ControllerInfo> {
        unsafe { read_inner(index) }.ok().flatten()
    }

    unsafe fn read_inner(index: u16) -> io::Result<Option<ControllerInfo>> {
        let guard = open_control(2)?;
        let fd = guard.0;
        write_command(fd, MGMT_OP_READ_INFO, index, &[])?;

        let mut buf = [0u8; 1024];
        for _ in 0..16 {
            let n = libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
            if n < 6 {
                return Ok(None);
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
                        return Ok(None);
                    }
                    // mgmt_rp_read_info: bdaddr(6), version(1), manufacturer(2, LE), ...
                    let rp = &params[3..];
                    if rp.len() < 9 {
                        return Ok(None);
                    }
                    return Ok(Some(ControllerInfo {
                        hci_version: rp[6],
                        manufacturer: u16::from_le_bytes([rp[7], rp[8]]),
                    }));
                }
            }
        }
        Ok(None)
    }

    pub fn set_public_address(index: u16, addr: [u8; 6]) -> Result<(), SetAddressError> {
        // mgmt carries bdaddr_t little-endian (LSB first); our input is display
        // order (MSB first), so reverse before sending.
        run_command(MGMT_OP_SET_PUBLIC_ADDRESS, index, &bdaddr_le(addr))
    }

    pub fn set_powered(index: u16, on: bool) -> Result<(), SetAddressError> {
        run_command(MGMT_OP_SET_POWERED, index, &[on as u8])
    }

    /// Send a command and wait for its terminating Command Complete / Command
    /// Status, mapping a non-zero status to [`SetAddressError::Command`].
    fn run_command(opcode: u16, index: u16, params: &[u8]) -> Result<(), SetAddressError> {
        unsafe { run_command_inner(opcode, index, params) }
    }

    unsafe fn run_command_inner(
        opcode: u16,
        index: u16,
        params: &[u8],
    ) -> Result<(), SetAddressError> {
        let guard = open_control(2)?;
        let fd = guard.0;
        write_command(fd, opcode, index, params)?;

        let mut buf = [0u8; 1024];
        for _ in 0..16 {
            let n = libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len());
            if n < 6 {
                return Err(SetAddressError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short or timed-out mgmt reply",
                )));
            }
            let n = n as usize;

            let event_code = u16::from_le_bytes([buf[0], buf[1]]);
            let param_len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
            if 6 + param_len > n {
                continue;
            }
            let params = &buf[6..6 + param_len];

            // Both Command Complete and Command Status begin with the originating
            // opcode(2, LE) then a status(1) byte; that is all we need here.
            let is_terminal =
                event_code == MGMT_EV_CMD_COMPLETE || event_code == MGMT_EV_CMD_STATUS;
            if is_terminal && params.len() >= 3 {
                let cmd_opcode = u16::from_le_bytes([params[0], params[1]]);
                let status = params[2];
                if cmd_opcode == opcode {
                    return if status == 0 {
                        Ok(())
                    } else {
                        Err(SetAddressError::Command(status))
                    };
                }
            }
        }
        Err(SetAddressError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "no mgmt command-complete for the requested opcode",
        )))
    }

    #[cfg(test)]
    mod wire_tests {
        use super::*;

        /// Regression guard for the opcode bug: `Set Public Address` is mgmt
        /// command 0x0039. We once used 0x0050 (a different, parameterless
        /// command), so the kernel rejected the 6-byte payload with
        /// INVALID_PARAMS (0x0d). This pins both the opcode and the full wire
        /// framing — header (little-endian opcode/index/len) plus the address in
        /// little-endian (reversed from display order).
        #[test]
        fn set_public_address_command_is_framed_correctly() {
            assert_eq!(MGMT_OP_SET_PUBLIC_ADDRESS, 0x0039);

            // Display order 1E:03:E0:D2:10:CB -> little-endian on the wire.
            let addr = [0x1E, 0x03, 0xE0, 0xD2, 0x10, 0xCB];
            assert_eq!(bdaddr_le(addr), [0xCB, 0x10, 0xD2, 0xE0, 0x03, 0x1E]);

            let cmd = encode_command(MGMT_OP_SET_PUBLIC_ADDRESS, 0, &bdaddr_le(addr));
            assert_eq!(
                cmd,
                vec![
                    0x39, 0x00, // opcode 0x0039, little-endian
                    0x00, 0x00, // controller index 0
                    0x06, 0x00, // parameter length 6
                    0xCB, 0x10, 0xD2, 0xE0, 0x03, 0x1E, // bdaddr, little-endian
                ]
            );
        }

        /// The other commands we send keep their (correct, already-working)
        /// opcodes and framing — a header-only command has param length 0.
        #[test]
        fn read_info_and_set_powered_framing() {
            assert_eq!(MGMT_OP_READ_INFO, 0x0004);
            assert_eq!(MGMT_OP_SET_POWERED, 0x0005);
            assert_eq!(
                encode_command(MGMT_OP_READ_INFO, 0, &[]),
                vec![0x04, 0x00, 0x00, 0x00, 0x00, 0x00]
            );
            assert_eq!(
                encode_command(MGMT_OP_SET_POWERED, 0, &[1]),
                vec![0x05, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01]
            );
        }
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

    #[test]
    fn set_address_on_bogus_index_errs_not_panics() {
        // Targets a clearly-invalid controller index so it can never touch a real
        // adapter. Without CAP_NET_ADMIN this fails at bind; with it, the kernel
        // returns an Invalid Index status. Either way: an error, never a panic.
        let r = set_public_address(0xfffe, [0x02, 0x00, 0x00, 0x11, 0x22, 0x33]);
        assert!(r.is_err());
    }

    #[test]
    fn status_str_covers_known_codes() {
        assert_eq!(SetAddressError::status_str(0x00), "success");
        assert!(SetAddressError::status_str(0x0C).contains("not supported"));
        assert_eq!(SetAddressError::status_str(0xff), "unknown status");
    }
}
