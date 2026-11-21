// Copyright © 2016-2026 The SENTIENT Authors

//! The Setup guide beside the Add instance form (the approved mockup's G1):
//! how to prepare a Linux or Windows server for the Platform Manager, with
//! the commands for the address, port and user typed in the form. The "Create
//! an SSH key" part follows this computer's own system: PowerShell on
//! Windows, a terminal elsewhere. The text is the documentation's
//! (remote-instances).

use gpui_kit::component::scroll::ScrollableElement as _;
use std::rc::Rc;

use gpui_kit::component::{ActiveTheme as _, Icon, IconName, h_flex, v_flex};

use gpui_kit::*;

use crate::remote::Os;

/// What the guide needs from the form.
pub struct Ctx {
    pub os: Os,
    pub host: String,
    pub port: String,
    pub user: String,
    /// The command copied last (its button shows a tick for a moment).
    pub copied: Option<String>,
    pub scroll: ScrollHandle,
    pub copy: Rc<dyn Fn(String, &mut App)>,
    pub close: Rc<dyn Fn(&mut App)>,
}

/// The guide pane's width beside the form.
pub const PANE_W: f32 = 500.;

/// Where the key box sits among the pane's children ("Create one" scrolls
/// there): after the intro and the three steps.
const KEY_INDEX: usize = 4;

pub fn pane(c: &Ctx, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let linux = c.os == Os::Linux;
    let host_windows = cfg!(windows);
    let user = if c.user.trim().is_empty() { "user".to_string() } else { c.user.trim().to_string() };
    let addr = if c.host.trim().is_empty() { "server-address".to_string() } else { c.host.trim().to_string() };
    let port = c.port.trim().parse::<u16>().ok().filter(|p| *p != 22);

    let sub = |t: &str| div().text_size(px(13.)).text_color(theme.muted_foreground).child(t.to_string());
    let text = |t: &str| div().text_sm().child(t.to_string());
    let tag = |pc: bool| {
        h_flex()
            .gap(px(6.))
            .items_center()
            .self_start()
            .px(px(8.))
            .py(px(1.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme.border)
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.muted_foreground)
            .child(Icon::new(if pc { gpui_kit::assets::IconName::Monitor } else { gpui_kit::assets::IconName::Server }).size(px(13.)))
            .child(if pc { "On this computer" } else { "On the server" })
    };
    let cmd = |line: String| {
        let n = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            line.hash(&mut h);
            h.finish()
        };
        let done = c.copied.as_deref() == Some(line.as_str());
        let copy = c.copy.clone();
        let l2 = line.clone();
        h_flex()
            .gap_2()
            .items_center()
            .min_h(px(36.))
            .pl(px(12.))
            .pr(px(4.))
            .py(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .child(div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).text_size(px(12.5)).line_height(px(19.)).child(line))
            .child(
                div()
                    .id(SharedString::from(format!("guide-copy-{n}")))
                    .size(px(28.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(if done { theme.primary } else { theme.muted_foreground })
                    .hover(|d| d.bg(theme.list_hover))
                    .child(Icon::new(if done { IconName::Check } else { IconName::Copy }).size(px(15.)))
                    .tooltip(crate::ui::tooltip("Copy"))
                    .on_click(move |_, _, cx| copy(l2.clone(), cx)),
            )
    };
    let step = |num: &str, title: &str, body: Vec<AnyElement>| {
        h_flex()
            .gap(px(12.))
            .items_start()
            .child(
                div()
                    .size(px(26.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(theme.sidebar_accent)
                    .text_color(theme.primary)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(13.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(num.to_string()),
            )
            .child(v_flex().flex_1().min_w_0().gap(px(8.)).child(div().text_sm().font_weight(FontWeight::SEMIBOLD).line_height(px(26.)).child(title.to_string())).children(body))
    };

    // The intro, with "Create one" taking the reader to the key box.
    let scroll = c.scroll.clone();
    let intro = |who: &str| {
        let scroll = scroll.clone();
        v_flex()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(div().min_w_0().child(format!("The Platform Manager reaches the server over SSH. {who} No SSH key yet?")))
            .child(
                div()
                    .id("guide-jump")
                    .text_color(theme.primary)
                    .font_weight(FontWeight::MEDIUM)
                    .underline()
                    .cursor_pointer()
                    .self_start()
                    .child("Create one.")
                    .on_click(move |_, _, _| scroll.scroll_to_item(KEY_INDEX)),
            )
    };

    // Create an SSH key: this computer's own system.
    let kp = if host_windows { "$env:USERPROFILE\\.ssh\\sentient_spm".to_string() } else { "~/.ssh/sentient_spm".to_string() };
    let gen = if host_windows { format!("ssh-keygen -t ed25519 -f \"{kp}\"") } else { format!("ssh-keygen -t ed25519 -f {kp}") };
    let ssh_p = port.map(|p| format!("-p {p} ")).unwrap_or_default();
    let put: Vec<AnyElement> = if linux {
        vec![
            tag(true).into_any_element(),
            if host_windows {
                cmd(format!("type \"{kp}.pub\" | ssh {ssh_p}{user}@{addr} \"mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys\""))
            } else {
                cmd(format!("ssh-copy-id {ssh_p}-i {kp}.pub {user}@{addr}"))
            }
            .into_any_element(),
            sub("It asks for the user's password once.").into_any_element(),
        ]
    } else {
        vec![
            tag(true).into_any_element(),
            cmd(if host_windows { format!("Get-Content \"{kp}.pub\" | Set-Clipboard") } else { format!("cat {kp}.pub") }).into_any_element(),
            sub(&format!("{} Then, in PowerShell as administrator:", if host_windows { "This copies the public key." } else { "Copy the line it shows." })).into_any_element(),
            tag(false).into_any_element(),
            cmd("Add-Content -Path C:\\ProgramData\\ssh\\administrators_authorized_keys -Value 'paste the key here'".into()).into_any_element(),
            cmd("icacls C:\\ProgramData\\ssh\\administrators_authorized_keys /inheritance:r /grant \"Administrators:F\" /grant \"SYSTEM:F\"".into()).into_any_element(),
            sub("Windows reads an administrator's keys from this file, not from the user's own folder.").into_any_element(),
        ]
    };
    let key_path = if host_windows {
        format!("{}\\.ssh\\sentient_spm", std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\you".into()))
    } else {
        format!("{}/.ssh/sentient_spm", std::env::var("HOME").unwrap_or_else(|_| "/home/you".into()))
    };
    let key_box = v_flex()
        .gap(px(14.))
        .px(px(16.))
        .py(px(14.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .child(h_flex().gap_2().items_center().font_weight(FontWeight::SEMIBOLD).child(Icon::new(gpui_kit::assets::IconName::KeyRound).size(px(16.)).text_color(theme.primary)).child("Create an SSH key"))
        .child(sub("Skip this if you already have a key, or if you sign in with a password."))
        .child(step(
            "a",
            "Make the key",
            vec![
                tag(true).into_any_element(),
                sub(if host_windows { "In PowerShell:" } else { "In a terminal:" }).into_any_element(),
                cmd(gen).into_any_element(),
                sub("Press Enter to save it there. A passphrase is optional; if you set one, enter it as Key passphrase.").into_any_element(),
            ],
        ))
        .child(step("b", "Put the public key on the server", put))
        .child(step(
            "c",
            "Choose the key in the form",
            vec![
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .items_center()
                    .child(text("Key file:"))
                    .child(div().px(px(5.)).rounded(px(4.)).border_1().border_color(theme.border).bg(theme.muted).font_family(theme.mono_font_family.clone()).text_size(px(12.5)).child(key_path))
                    .into_any_element(),
                sub("Choose the file without .pub. The .pub file is the one that goes on the server.").into_any_element(),
            ],
        ));

    let fingerprint = |path: &str| {
        step(
            "4",
            "Check the key fingerprint",
            vec![
                sub("When you select Connect, the Platform Manager shows the server's fingerprint. Compare it with:").into_any_element(),
                tag(false).into_any_element(),
                cmd(format!("ssh-keygen -lf {path}")).into_any_element(),
            ],
        )
    };
    let body: Vec<AnyElement> = if linux {
        vec![
            intro("Sign in as root, or as a user that may use sudo.").into_any_element(),
            step(
                "1",
                "Install the SSH server",
                vec![
                    tag(false).into_any_element(),
                    sub("Debian and Ubuntu:").into_any_element(),
                    cmd("sudo apt install openssh-server".into()).into_any_element(),
                    sub("Red Hat, Rocky, Alma and Fedora:").into_any_element(),
                    cmd("sudo dnf install openssh-server".into()).into_any_element(),
                ],
            )
            .into_any_element(),
            step("2", "Start it now and with the system", vec![tag(false).into_any_element(), cmd("sudo systemctl enable --now ssh".into()).into_any_element(), sub("On the Red Hat family the service is called sshd.").into_any_element()]).into_any_element(),
            step(
                "3",
                &format!("Allow TCP port {}", port.unwrap_or(22)),
                vec![tag(false).into_any_element(), sub("Only if a firewall is on. For example:").into_any_element(), cmd(format!("sudo ufw allow {}/tcp", port.unwrap_or(22))).into_any_element()],
            )
            .into_any_element(),
            key_box.into_any_element(),
            fingerprint("/etc/ssh/ssh_host_ed25519_key.pub").into_any_element(),
            div()
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .bg(theme.warning)
                .text_color(theme.warning_foreground)
                .text_size(px(13.))
                .child("When sudo asks for a password and you sign in with a key, enter that password as sudo password.")
                .into_any_element(),
        ]
    } else {
        vec![
            intro("Sign in with an account in the Administrators group.").into_any_element(),
            step(
                "1",
                "Turn on OpenSSH Server",
                vec![
                    tag(false).into_any_element(),
                    sub("Settings, System, Optional features, Add a feature, OpenSSH Server. Or in PowerShell as administrator:").into_any_element(),
                    cmd("Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0".into()).into_any_element(),
                ],
            )
            .into_any_element(),
            step("2", "Start it now and with Windows", vec![tag(false).into_any_element(), cmd("Start-Service sshd\nSet-Service sshd -StartupType Automatic".into()).into_any_element()]).into_any_element(),
            step(
                "3",
                "Check the firewall",
                vec![
                    tag(false).into_any_element(),
                    sub("Installing the feature adds the rule OpenSSH SSH Server (sshd), which allows TCP port 22. To check it:").into_any_element(),
                    cmd("Get-NetFirewallRule -Name *OpenSSH-Server* | Select-Object Name, Enabled".into()).into_any_element(),
                ],
            )
            .into_any_element(),
            key_box.into_any_element(),
            fingerprint("C:\\ProgramData\\ssh\\ssh_host_ed25519_key.pub").into_any_element(),
        ]
    };

    let close = c.close.clone();
    v_flex()
        .size_full()
        .border_l_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .flex_shrink_0()
                .gap(px(12.))
                .items_center()
                .pl(px(20.))
                .pr(px(16.))
                .py(px(12.))
                .border_b_1()
                .border_color(theme.border)
                .child(Icon::new(IconName::BookOpen).size(px(16.)).text_color(theme.primary))
                .child(div().flex_1().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(format!("Prepare a {} server", if linux { "Linux" } else { "Windows" })))
                .child(
                    div()
                        .id("guide-close")
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_color(theme.muted_foreground)
                        .hover(|d| d.bg(theme.list_hover))
                        .child(Icon::new(IconName::Close).size(px(16.)))
                        .tooltip(crate::ui::tooltip("Close the guide"))
                        .on_click(move |_, _, cx| close(cx)),
                ),
        )
        .child(
            // It scrolls on its own; the form stays as it is.
            div().relative().flex_1().min_h_0().child(
                v_flex()
                    .id("guide-steps")
                    .absolute()
                    .inset_0()
                    .track_scroll(&c.scroll)
                    .overflow_y_scroll() /* scroll-own: a box sized by its content or scrolled from code; its own bar is drawn beside it where it has a handle */
                    .px(px(20.))
                    .pt(px(16.))
                    .pb(px(20.))
                    .gap(px(18.))
                    .children(body),
            ),
        )
        .into_any_element()
}
