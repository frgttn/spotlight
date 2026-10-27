//! Joining a Wi-Fi network a QR code shared: its profile is saved to
//! Windows (as Settings would after typing the password) with the WLAN API,
//! then Windows is asked to connect with it.

use std::{ffi::c_void, ptr};

use spotlight_core::item::{WifiNetwork, WifiSecurity};
use windows::{
    Win32::{
        Foundation::{ERROR_ACCESS_DENIED, HANDLE},
        NetworkManagement::WiFi::{
            WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO_LIST, WLAN_PROFILE_USER,
            WlanCloseHandle, WlanConnect, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle,
            WlanSetProfile, dot11_BSS_type_any, wlan_connection_mode_profile,
        },
    },
    core::{GUID, HSTRING, PCWSTR},
};

/// The WLAN API version of Windows Vista and later.
const CLIENT_VERSION: u32 = 2;

/// Saves `network` and starts connecting to it on the first Wi-Fi adapter.
/// Windows connects in the background; `Ok` says it was asked to.
pub fn connect(network: &WifiNetwork) -> Result<Option<String>, String> {
    let profile = profile_xml(network);
    unsafe {
        let mut version = 0;
        let mut handle = HANDLE::default();
        check(
            WlanOpenHandle(CLIENT_VERSION, None, &mut version, &mut handle),
            "Wi-Fi isn't available",
        )?;
        let connected = connect_with(handle, network, &profile);
        WlanCloseHandle(handle, None);
        connected
    }
}

unsafe fn connect_with(
    handle: HANDLE,
    network: &WifiNetwork,
    profile: &str,
) -> Result<Option<String>, String> {
    unsafe {
        let interface = first_interface(handle)?;
        let xml = HSTRING::from(profile);
        let mut reason = 0;
        // For every user, as Settings saves them; where policy keeps that to
        // administrators, for this user only.
        let mut saved = WlanSetProfile(handle, &interface, 0, &xml, None, true, None, &mut reason);
        if saved == ERROR_ACCESS_DENIED.0 {
            saved = WlanSetProfile(
                handle,
                &interface,
                WLAN_PROFILE_USER,
                &xml,
                None,
                true,
                None,
                &mut reason,
            );
        }
        check(saved, "Couldn't save the network")?;
        let name = HSTRING::from(network.ssid.as_str());
        let parameters = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(name.as_ptr()),
            pDot11Ssid: ptr::null_mut(),
            pDesiredBssidList: ptr::null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        };
        check(
            WlanConnect(handle, &interface, &parameters, None),
            "Couldn't connect",
        )?;
        Ok(Some(format!("Connecting to {}…", network.ssid)))
    }
}

/// The first wireless adapter's id.
unsafe fn first_interface(handle: HANDLE) -> Result<GUID, String> {
    unsafe {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = ptr::null_mut();
        check(
            WlanEnumInterfaces(handle, None, &mut list),
            "Couldn't list the Wi-Fi adapters",
        )?;
        let first = (*list)
            .InterfaceInfo
            .as_ptr()
            .cast::<windows::Win32::NetworkManagement::WiFi::WLAN_INTERFACE_INFO>();
        let interface = ((*list).dwNumberOfItems > 0).then(|| (*first).InterfaceGuid);
        WlanFreeMemory(list as *const c_void);
        interface.ok_or_else(|| "There's no Wi-Fi adapter".into())
    }
}

fn check(code: u32, what: &str) -> Result<(), String> {
    match code {
        0 => Ok(()),
        code => Err(format!(
            "{what}: {}",
            windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(code)).message()
        )),
    }
}

/// The WLAN profile XML for `network`.
fn profile_xml(network: &WifiNetwork) -> String {
    let name = escape(&network.ssid);
    let hex: String = network
        .ssid
        .bytes()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    let (authentication, encryption) = match network.security {
        WifiSecurity::Open => ("open", "none"),
        WifiSecurity::Wep => ("open", "WEP"),
        WifiSecurity::Wpa => ("WPA2PSK", "AES"),
        WifiSecurity::Wpa3 => ("WPA3SAE", "AES"),
    };
    let password = &network.password;
    // 64 hex digits (or a WEP key) is the key itself; anything else a passphrase.
    let key_type = if network.security == WifiSecurity::Wep
        || (password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        "networkKey"
    } else {
        "passPhrase"
    };
    let shared_key = if network.security == WifiSecurity::Open {
        String::new()
    } else {
        format!(
            "<sharedKey><keyType>{key_type}</keyType><protected>false</protected>\
             <keyMaterial>{}</keyMaterial></sharedKey>",
            escape(password)
        )
    };
    format!(
        "<?xml version=\"1.0\"?>\
         <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
         <name>{name}</name>\
         <SSIDConfig><SSID><hex>{hex}</hex><name>{name}</name></SSID>\
         <nonBroadcast>{hidden}</nonBroadcast></SSIDConfig>\
         <connectionType>ESS</connectionType><connectionMode>auto</connectionMode>\
         <MSM><security><authEncryption><authentication>{authentication}</authentication>\
         <encryption>{encryption}</encryption><useOneX>false</useOneX></authEncryption>\
         {shared_key}</security></MSM></WLANProfile>",
        hidden = network.hidden,
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_escape_and_pick_the_key_type() {
        let xml = profile_xml(&WifiNetwork {
            ssid: "Café <5G>".into(),
            security: WifiSecurity::Wpa,
            password: "a&b".into(),
            hidden: true,
        });
        assert!(xml.contains("<name>Café &lt;5G&gt;</name>"), "{xml}");
        assert!(xml.contains("<hex>436166C3A9203C35473E</hex>"), "{xml}");
        assert!(xml.contains("<nonBroadcast>true</nonBroadcast>"));
        assert!(xml.contains("<authentication>WPA2PSK</authentication>"));
        assert!(xml.contains("<keyType>passPhrase</keyType>"));
        assert!(xml.contains("<keyMaterial>a&amp;b</keyMaterial>"));

        let open = profile_xml(&WifiNetwork {
            ssid: "Free".into(),
            security: WifiSecurity::Open,
            ..Default::default()
        });
        assert!(open.contains("<encryption>none</encryption>"));
        assert!(!open.contains("sharedKey"));
    }
}
