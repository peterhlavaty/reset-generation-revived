//! LAN-Setup.exe: lets this PC host LAN games (Windows Firewall).
//!
//! The PC that hosts a game without internet (or with server=lan) runs the relay, rg-relay.exe,
//! on port 8787: TCP for the game, UDP for the other players' search. This lets both in from the
//! local network only (LocalSubnet), in every network type, so it does not matter whether Windows
//! calls the network public or private. It also removes the inbound Block rules Windows creates
//! for the game or the relay when its firewall prompt is cancelled (a Block rule wins over any
//! Allow rule). Joining players need nothing; running it again is harmless.
//! `LAN-Setup.exe --remove` deletes the rules.

#[cfg(not(windows))]
fn main() {
    eprintln!("LAN-Setup is for Windows (on other systems open TCP and UDP port 8787 to the LAN yourself).");
}

#[cfg(windows)]
fn main() {
    let remove = std::env::args().any(|a| a.eq_ignore_ascii_case("--remove") || a.eq_ignore_ascii_case("-remove"));
    if !win::is_admin() {
        // ask for administrator rights: start again elevated, in its own console
        if !win::relaunch_elevated(remove) {
            println!("Administrator rights are needed to change the Windows Firewall settings.");
            wait_close();
        }
        return;
    }
    match win::configure(remove) {
        Ok(lines) => lines.iter().for_each(|l| println!("{l}")),
        Err(e) => println!("Failed: {e}"),
    }
    wait_close();
}

#[cfg(windows)]
fn wait_close() {
    println!();
    println!("Press Enter to close");
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
}

#[cfg(windows)]
mod win {
    use windows::core::{Interface, BSTR, HSTRING, PCWSTR};
    use windows::Win32::NetworkManagement::WindowsFirewall::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Ole::IEnumVARIANT;
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    const GROUP: &str = "Reset Generation Revived";
    const PORT: u16 = 8787;
    /// Programs whose Block rules (left by a cancelled firewall prompt) keep LAN players out.
    const PROGRAMS: [&str; 2] = ["\\resetgeneration-revived.exe", "\\rg-relay.exe"];

    pub fn is_admin() -> bool {
        unsafe { IsUserAnAdmin().as_bool() }
    }

    pub fn relaunch_elevated(remove: bool) -> bool {
        let Ok(exe) = std::env::current_exe() else { return false };
        let exe = HSTRING::from(exe.as_os_str());
        let args = HSTRING::from(if remove { "--remove" } else { "" });
        let r = unsafe { ShellExecuteW(None, &HSTRING::from("runas"), &exe, &args, PCWSTR::null(), SW_SHOWNORMAL) };
        r.0 as isize > 32
    }

    fn rule_name(proto: &str) -> String {
        format!("{GROUP} LAN ({proto} {PORT})")
    }

    pub fn configure(remove: bool) -> windows::core::Result<Vec<String>> {
        let mut out = Vec::new();
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)?;
            let rules = policy.Rules()?;
            // our rules (by name; Remove takes one at a time)
            let mut ours = Vec::new();
            let mut blocked = Vec::new();
            let en: IEnumVARIANT = rules._NewEnum()?.cast()?;
            loop {
                let mut v = [VARIANT::default()];
                let mut got = 0u32;
                if en.Next(&mut v, &mut got).is_err() || got == 0 {
                    break;
                }
                let Ok(disp) = windows::Win32::System::Com::IDispatch::try_from(&v[0]) else { continue };
                let Ok(rule) = disp.cast::<INetFwRule>() else { continue };
                let name = rule.Name().map(|b| b.to_string()).unwrap_or_default();
                let group = rule.Grouping().map(|b| b.to_string()).unwrap_or_default();
                if group == GROUP {
                    ours.push(name);
                    continue;
                }
                let app = rule.ApplicationName().map(|b| b.to_string().to_lowercase()).unwrap_or_default();
                if !remove
                    && rule.Direction().ok() == Some(NET_FW_RULE_DIR_IN)
                    && rule.Action().ok() == Some(NET_FW_ACTION_BLOCK)
                    && PROGRAMS.iter().any(|p| app.ends_with(p))
                {
                    blocked.push(name);
                }
            }
            for n in &ours {
                rules.Remove(&BSTR::from(n.as_str()))?;
            }
            if remove {
                out.push("Removed the Reset Generation Revived LAN rules.".into());
                return Ok(out);
            }
            for n in &blocked {
                rules.Remove(&BSTR::from(n.as_str()))?;
                out.push(format!("Removed a rule that blocked the game: {n}"));
            }
            for (proto, num) in [("TCP", NET_FW_IP_PROTOCOL_TCP.0), ("UDP", NET_FW_IP_PROTOCOL_UDP.0)] {
                let r: INetFwRule = CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER)?;
                r.SetName(&BSTR::from(rule_name(proto)))?;
                r.SetGrouping(&BSTR::from(GROUP))?;
                r.SetDescription(&BSTR::from("LAN games of Reset Generation Revived (the hosting PC's relay)"))?;
                r.SetDirection(NET_FW_RULE_DIR_IN)?;
                r.SetProtocol(num)?;
                r.SetLocalPorts(&BSTR::from(PORT.to_string()))?;
                r.SetRemoteAddresses(&BSTR::from("LocalSubnet"))?;
                r.SetProfiles(NET_FW_PROFILE2_ALL.0)?;
                r.SetAction(NET_FW_ACTION_ALLOW)?;
                r.SetEnabled(windows::Win32::Foundation::VARIANT_TRUE)?;
                rules.Add(&r)?;
            }
            out.push(format!("Done: this PC can host LAN games (TCP and UDP {PORT} open to the local network)."));
            out.push("To host: start the game and press F5 in the main menu.".into());
            out.push(String::new());
            out.push("An antivirus with its own firewall (ESET, Norton, McAfee...) replaces Windows Firewall:".into());
            out.push(format!("allow rg-relay.exe (or TCP and UDP port {PORT} from the local network) there too."));
        }
        Ok(out)
    }
}
