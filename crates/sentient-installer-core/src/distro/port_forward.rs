// Copyright © 2016-2026 The SENTIENT Authors

//! Windows: SENTIENT's TCP ports, forwarded from every address of this
//! computer to its WSL distro (`netsh interface portproxy`), and whether
//! SENTIENT answers the way another computer reaches it.
//!
//! WSL 2 gives the distro a new eth0 address at every start of its virtual
//! machine; WSL itself forwards a published port to Windows' own localhost
//! only. The keep-alive task writes the forward when it starts
//! ([`super::keepalive_command`]); every start from the Platform Manager
//! writes it again here once the distro is up ([`refresh`], from
//! [`super::wake_keepalive`]), so a forward to an earlier address never
//! outlives a start.
//!
//! The pure parts (reading `ip` and `netsh` output, the commands, the order
//! of a start's steps) build and are tested on every platform.

use super::DeployConfig;
use crate::progress::ProgressFn;

/// What the keep-alive task is doing, as `schtasks /query` tells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// No such task (a stack in the operator's own distro, none installed).
    Missing,
    /// Registered, not running: SENTIENT's distro is down.
    Ready,
    /// Running: it holds the distro up.
    Running,
}

/// One step of bringing SENTIENT's own distro up for a start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeStep {
    /// Run the keep-alive task (it boots the distro and holds it).
    RunTask,
    /// Wait until Docker in the distro answers.
    WaitDocker,
    /// Point the forwarded ports at the distro's address of now.
    ForwardPorts,
}

/// The steps of [`super::wake_keepalive`], in order. The forward is
/// written again also when the task already runs: its own forward was
/// written when it started, and the distro may have got another address
/// since (the 2026-11-18 rc3 VM pass looked for exactly this).
pub fn wake_steps(task: Task) -> Vec<WakeStep> {
    match task {
        Task::Missing => Vec::new(),
        Task::Ready => vec![WakeStep::RunTask, WakeStep::WaitDocker, WakeStep::ForwardPorts],
        Task::Running => vec![WakeStep::ForwardPorts],
    }
}

/// The task's state from `schtasks /query /fo csv /nh` (None: the query
/// failed, no task). The status column is localized only in its text
/// ("Running" on English Windows); a running task is the one we look for.
pub fn task_from_query(ok: bool, csv: &str) -> Task {
    match ok {
        false => Task::Missing,
        true if csv.contains("Running") => Task::Running,
        true => Task::Ready,
    }
}

/// eth0's IPv4 address from `ip -4 -o addr show eth0`
/// ("2: eth0    inet 172.19.134.88/20 brd … scope global eth0 …").
pub fn parse_eth0(out: &str) -> Option<String> {
    let mut words = out.split_whitespace();
    while let Some(w) = words.next() {
        if w == "inet" {
            let addr = words.next()?.split('/').next()?;
            return addr.parse::<std::net::Ipv4Addr>().ok().map(|a| a.to_string());
        }
    }
    None
}

/// The first IPv4 address of `hostname -I`: eth0 in WSL (Docker's bridges
/// come later). The fallback when `ip` gives nothing.
pub fn parse_hostname_i(out: &str) -> Option<String> {
    out.split_whitespace().find_map(|w| w.parse::<std::net::Ipv4Addr>().ok()).map(|a| a.to_string())
}

/// One `netsh interface portproxy show v4tov4` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Forward {
    pub listen_address: String,
    pub listen_port: u16,
    pub connect_address: String,
    pub connect_port: u16,
}

/// The rows of `netsh interface portproxy show v4tov4`: four columns, the
/// two ports numeric. Headers (localized) and dashes are skipped.
pub fn parse_portproxy(out: &str) -> Vec<Forward> {
    out.lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            let [la, lp, ca, cp] = w.as_slice() else { return None };
            Some(Forward { listen_address: la.to_string(), listen_port: lp.parse().ok()?, connect_address: ca.to_string(), connect_port: cp.parse().ok()? })
        })
        .collect()
}

/// The ports of `ports` whose forward from every address (0.0.0.0) does
/// not go to `ip` on the same port: missing, or to another address.
pub fn stale_ports(ip: &str, ports: &[u16], current: &[Forward]) -> Vec<u16> {
    ports
        .iter()
        .copied()
        .filter(|p| !current.iter().any(|f| f.listen_address == "0.0.0.0" && f.listen_port == *p && f.connect_address == ip && f.connect_port == *p))
        .collect()
}

/// The `netsh` arguments that point `port` at `ip`: the old forward
/// removed first, then the new one (an `add` over an existing listen
/// address and port keeps the old target on some Windows builds).
pub fn forward_args(ip: &str, port: u16) -> [Vec<String>; 2] {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let listen = format!("listenport={port}");
    [
        s(&["interface", "portproxy", "delete", "v4tov4", &listen, "listenaddress=0.0.0.0"]),
        s(&["interface", "portproxy", "add", "v4tov4", &listen, "listenaddress=0.0.0.0", &format!("connectport={port}"), &format!("connectaddress={ip}")]),
    ]
}

/// This computer's address toward its network (the one other computers
/// use): the source address of a route to a documentation address. No
/// packet is sent. None without a network route.
pub fn lan_address() -> Option<std::net::Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(a) if !a.is_unspecified() && !a.is_loopback() => Some(a),
        _ => None,
    }
}

/// Whether SENTIENT answers on `port` the way another computer reaches
/// it: on this computer's network address, through the forward (a
/// forward to an earlier address of the distro does not answer there,
/// while Windows' own localhost may). Without a network: localhost.
pub fn answers_from_outside(port: u16) -> bool {
    match lan_address() {
        Some(a) => crate::native::http_answers_on(&a.to_string(), port),
        None => crate::native::http_answers(port),
    }
}

/// Windows, SENTIENT's own distro: its eth0 address of now. The distro
/// may just be starting: asked a few times.
#[cfg(windows)]
pub fn distro_address() -> Option<String> {
    use crate::sys;
    let ask = |args: &[&str]| {
        if super::left_out(args) {
            return None;
        }
        sys::output_wsl("wsl.exe", args, std::time::Duration::from_secs(30)).filter(|(ok, _, _)| *ok).map(|(_, out, _)| sys::decode(&out))
    };
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        if let Some(ip) = ask(&["-d", super::DISTRO, "-u", "root", "--", "ip", "-4", "-o", "addr", "show", "eth0"]).as_deref().and_then(parse_eth0) {
            return Some(ip);
        }
        if let Some(ip) = ask(&["-d", super::DISTRO, "-u", "root", "--", "hostname", "-I"]).as_deref().and_then(parse_hostname_i) {
            return Some(ip);
        }
    }
    None
}

/// Windows, SENTIENT's own distro: point the forwarded ports at the
/// distro's address of now, changing only the ones that point elsewhere.
/// Ok(true) when something changed. Needs administrator rights (the
/// Platform Manager runs elevated).
#[cfg(windows)]
pub fn refresh(sink: &ProgressFn, cfg: &DeployConfig) -> Result<bool, String> {
    use crate::progress::Progress;
    use crate::sys;
    if cfg.wsl_distro.is_some() || cfg.external.is_some() {
        return Ok(false);
    }
    let ports = super::forwarded_ports(cfg);
    let ip = distro_address().ok_or_else(|| "the address of SENTIENT's WSL distro could not be read".to_string())?;
    let shown = sys::output("netsh.exe", &["interface", "portproxy", "show", "v4tov4"]).map(|(_, out, _)| sys::decode(&out)).unwrap_or_default();
    let stale = stale_ports(&ip, &ports, &parse_portproxy(&shown));
    if stale.is_empty() {
        sink(Progress::Log { line: format!("Ports {} go to SENTIENT's distro at {ip}", join(&ports)) });
        return Ok(false);
    }
    for p in &stale {
        let [del, add] = forward_args(&ip, *p);
        let del: Vec<&str> = del.iter().map(String::as_str).collect();
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        let _ = sys::output("netsh.exe", &del);
        match sys::output("netsh.exe", &add) {
            Some((true, _, _)) => {}
            Some((false, out, err)) => return Err(format!("netsh could not forward port {p}: {} {}", sys::decode(&err).trim(), sys::decode(&out).trim())),
            None => return Err("could not run netsh.exe".into()),
        }
    }
    sink(Progress::Log { line: format!("Ports {} now go to SENTIENT's distro at {ip} (its address changed)", join(&stale)) });
    Ok(true)
}

#[cfg(not(windows))]
pub fn refresh(_sink: &ProgressFn, _cfg: &DeployConfig) -> Result<bool, String> {
    Ok(false)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn join(ports: &[u16]) -> String {
    ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_start_forwards_the_ports_after_the_distro_is_up_also_when_the_task_runs() {
        assert_eq!(wake_steps(Task::Ready), vec![WakeStep::RunTask, WakeStep::WaitDocker, WakeStep::ForwardPorts]);
        // The task already holds the distro: its forward may be old.
        assert_eq!(wake_steps(Task::Running), vec![WakeStep::ForwardPorts]);
        assert!(wake_steps(Task::Missing).is_empty());
        assert_eq!(task_from_query(true, "\"\\SENTIENT Docker\",\"N/A\",\"Running\"\r\n"), Task::Running);
        assert_eq!(task_from_query(true, "\"\\SENTIENT Docker\",\"N/A\",\"Ready\"\r\n"), Task::Ready);
        assert_eq!(task_from_query(false, ""), Task::Missing);
    }

    #[test]
    fn the_distros_address_is_eth0s() {
        let ip = "2: eth0    inet 172.19.134.88/20 brd 172.19.143.255 scope global eth0\\       valid_lft forever preferred_lft forever\n";
        assert_eq!(parse_eth0(ip).as_deref(), Some("172.19.134.88"));
        assert_eq!(parse_eth0(""), None);
        assert_eq!(parse_eth0("Device \"eth0\" does not exist."), None);
        // hostname -I: eth0 first, Docker's bridges after.
        assert_eq!(parse_hostname_i("172.31.228.33 10.213.0.1 10.214.0.1 \n").as_deref(), Some("172.31.228.33"));
        assert_eq!(parse_hostname_i("fe80::1 172.18.228.51").as_deref(), Some("172.18.228.51"));
        assert_eq!(parse_hostname_i("\n"), None);
    }

    #[test]
    fn only_the_forwards_to_another_address_are_written_again() {
        let shown = "\r\nListen on ipv4:             Connect to ipv4:\r\n\r\nAddress         Port        Address         Port\r\n--------------- ----------  --------------- ----------\r\n0.0.0.0         8080        172.26.65.18    8080\r\n0.0.0.0         1883        172.19.134.88   1883\r\n127.0.0.1       8883        172.19.134.88   8883\r\n\r\n";
        let rows = parse_portproxy(shown);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], Forward { listen_address: "0.0.0.0".into(), listen_port: 8080, connect_address: "172.26.65.18".into(), connect_port: 8080 });
        // 8080 goes to the previous boot's address, 8883 is not forwarded
        // from every address: both written again; 1883 is right.
        assert_eq!(stale_ports("172.19.134.88", &[8080, 1883, 8883], &rows), vec![8080, 8883]);
        assert!(stale_ports("172.19.134.88", &[1883], &rows).is_empty());
        assert_eq!(stale_ports("172.19.134.88", &[8080], &[]), vec![8080]);
        // A localized header has no numeric port columns.
        assert!(parse_portproxy("Abhören an IPv4:   Verbinden mit IPv4:\nAdresse Anschluss Adresse Anschluss\n").is_empty());
    }

    #[test]
    fn a_forward_is_removed_before_it_is_added_again() {
        let [del, add] = forward_args("172.19.134.88", 8080);
        assert_eq!(del.join(" "), "interface portproxy delete v4tov4 listenport=8080 listenaddress=0.0.0.0");
        assert_eq!(add.join(" "), "interface portproxy add v4tov4 listenport=8080 listenaddress=0.0.0.0 connectport=8080 connectaddress=172.19.134.88");
    }
}
