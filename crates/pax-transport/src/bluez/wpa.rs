//! A precise 802.1X / EAP [`StandardsResolver`](crate::backend::StandardsResolver)
//! backed by **wpa_supplicant**.
//!
//! Where the NetworkManager resolver reads a coarse NetworkManager *device* state,
//! wpa_supplicant exposes the actual supplicant state machine over D-Bus
//! (`fi.w1.wpa_supplicant1`) — including the association / 4-way-handshake /
//! completed transitions that an 802.1X EAP exchange drives. This resolver reads
//! each interface's `State` and maps it to a
//! [`PortAuthState`](pax_core::PortAuthState). When a Bluetooth PAN is bridged onto
//! an 802.1X-guarded port and wpa_supplicant runs the supplicant on that interface
//! (e.g. the `wired` driver), this is the precise source.
//!
//! Best-effort and Linux-only (feature `port-auth-wpa`): it needs wpa_supplicant
//! running with its D-Bus interface enabled.
//! [`resolve`](crate::backend::StandardsResolver::resolve) is synchronous, so the
//! state is cached and refreshed out-of-band, like the NM resolver.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pax_core::{DeviceId, PortAuthState, StandardsProfile};
use zbus::zvariant::OwnedObjectPath;
use zbus::Connection;

use crate::backend::StandardsResolver;
use crate::error::TransportError;

const WPA_SERVICE: &str = "fi.w1.wpa_supplicant1";
const WPA_ROOT_PATH: &str = "/fi/w1/wpa_supplicant1";
const WPA_ROOT_IFACE: &str = "fi.w1.wpa_supplicant1";
const WPA_IFACE_IFACE: &str = "fi.w1.wpa_supplicant1.Interface";

fn zerr(e: zbus::Error) -> TransportError {
    TransportError::Backend(format!("wpa_supplicant: {e}"))
}

/// Map a wpa_supplicant interface `State` string to a [`PortAuthState`].
fn map_state(state: &str) -> PortAuthState {
    match state {
        "completed" => PortAuthState::Authenticated,
        "authenticating" | "associating" | "associated" | "4way_handshake" | "group_handshake" => {
            PortAuthState::Authenticating
        }
        "disconnected" | "inactive" | "scanning" | "interface_disabled" => {
            PortAuthState::Unauthenticated
        }
        _ => PortAuthState::Unauthenticated,
    }
}

/// Precedence so the "most authenticated" interface wins when several exist.
fn rank(s: PortAuthState) -> u8 {
    match s {
        PortAuthState::Authenticated => 4,
        PortAuthState::Authenticating => 3,
        PortAuthState::Failed => 2,
        PortAuthState::Unauthenticated => 1,
        PortAuthState::NotApplicable => 0,
    }
}

/// A [`StandardsResolver`] backed by a cached wpa_supplicant interface state.
///
/// wpa_supplicant state is per-interface (not per-Bluetooth-device), so this
/// resolver applies one aggregate port-auth state to every peer — the state of the
/// configured interface, or the most-authenticated interface if none is pinned.
pub struct WpaSupplicantStandards {
    conn: Connection,
    /// Restrict to one interface (`Ifname`), or aggregate across all if `None`.
    ifname: Option<String>,
    state: Mutex<PortAuthState>,
}

impl WpaSupplicantStandards {
    /// Connect to the system bus. Missing wpa_supplicant surfaces later as
    /// [`PortAuthState::NotApplicable`].
    pub async fn connect() -> Result<Arc<Self>, TransportError> {
        let conn = Connection::system().await.map_err(zerr)?;
        Ok(Arc::new(WpaSupplicantStandards {
            conn,
            ifname: None,
            state: Mutex::new(PortAuthState::NotApplicable),
        }))
    }

    /// Restrict to a single interface by name (e.g. `"pan0"`, `"br0"`).
    pub fn for_interface(self: Arc<Self>, ifname: impl Into<String>) -> Arc<Self> {
        // Rebuild with the filter (Arc fields are otherwise immutable).
        Arc::new(WpaSupplicantStandards {
            conn: self.conn.clone(),
            ifname: Some(ifname.into()),
            state: Mutex::new(*self.state.lock().unwrap()),
        })
    }

    /// Refresh the cached state from wpa_supplicant's interface(s).
    pub async fn refresh(&self) -> Result<(), TransportError> {
        let root = zbus::Proxy::new(&self.conn, WPA_SERVICE, WPA_ROOT_PATH, WPA_ROOT_IFACE)
            .await
            .map_err(zerr)?;
        let interfaces: Vec<OwnedObjectPath> =
            root.get_property("Interfaces").await.map_err(zerr)?;

        let mut best = PortAuthState::NotApplicable;
        for path in interfaces {
            let iface =
                match zbus::Proxy::new(&self.conn, WPA_SERVICE, &path, WPA_IFACE_IFACE).await {
                    Ok(p) => p,
                    Err(_) => continue,
                };
            if let Some(want) = &self.ifname {
                let name: String = iface.get_property("Ifname").await.unwrap_or_default();
                if &name != want {
                    continue;
                }
            }
            let state: String = match iface.get_property("State").await {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mapped = map_state(&state);
            if rank(mapped) > rank(best) {
                best = mapped;
            }
        }

        *self.state.lock().unwrap() = best;
        Ok(())
    }

    /// Spawn a background task that calls [`Self::refresh`] every `interval`.
    pub fn spawn_auto_refresh(self: Arc<Self>, interval: Duration) {
        tokio::spawn(async move {
            loop {
                let _ = self.refresh().await;
                tokio::time::sleep(interval).await;
            }
        });
    }
}

impl StandardsResolver for WpaSupplicantStandards {
    fn resolve(&self, _peer: DeviceId) -> StandardsProfile {
        StandardsProfile::bredr_default().with_port_auth(*self.state.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_mapping() {
        assert_eq!(map_state("completed"), PortAuthState::Authenticated);
        assert_eq!(map_state("4way_handshake"), PortAuthState::Authenticating);
        assert_eq!(map_state("associating"), PortAuthState::Authenticating);
        assert_eq!(map_state("disconnected"), PortAuthState::Unauthenticated);
        assert_eq!(map_state("inactive"), PortAuthState::Unauthenticated);
        assert_eq!(map_state("weird"), PortAuthState::Unauthenticated);
    }

    #[test]
    fn most_authenticated_wins() {
        assert!(rank(PortAuthState::Authenticated) > rank(PortAuthState::Authenticating));
        assert!(rank(PortAuthState::Authenticating) > rank(PortAuthState::Unauthenticated));
    }
}
