//! Names of remote hosts (user bug 36). The desktop app also reports hosts it
//! does not reach over SSH (ChatGPT remote-control devices, WSL distributions);
//! a host is called by its own name (the app's display name, an ssh alias or
//! `user@host`), else by its kind ("Remote-control device"), and never by the
//! raw app `hostId`, which pages keep for a tooltip or a copy action.

use superba_core::{HostKind, RemoteHost, TargetId, target_title};

use crate::design::fonts::drawable;
use crate::design::icons::Icon;
use crate::i18n::Tr;
use crate::pages::Cx;

/// What to call `host` (see the module docs); characters Superba cannot draw
/// are left out of a name it did not write.
pub fn title(tr: &Tr, host: &RemoteHost) -> String {
    host.display_name()
        .map(drawable)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| kind(tr, host.kind))
}

/// Icon of a host kind: a server for SSH hosts, a monitor for remote-control
/// devices, a terminal for WSL distributions.
pub fn icon(kind: HostKind) -> Icon {
    match kind {
        HostKind::RemoteControl => Icon::Targets,
        HostKind::Wsl => Icon::Terminal,
        HostKind::Cloud => Icon::Cloud,
        _ => Icon::Server,
    }
}

/// Display name of a host kind ("远程控制设备").
pub fn kind(tr: &Tr, kind: HostKind) -> String {
    tr.text(&kind.text())
}

/// The host the app knows with this target id (the Remote hosts list, else the
/// overview's hosts).
pub fn find<'a>(cx: &'a Cx<'_>, id: &TargetId) -> Option<&'a RemoteHost> {
    let listed = cx
        .data
        .hosts
        .value
        .as_deref()
        .and_then(|hosts| hosts.iter().find(|h| &h.host.target_id == id))
        .map(|h| &h.host);
    listed.or_else(|| {
        cx.data
            .overview
            .value
            .as_ref()
            .and_then(|o| o.remotes.iter().find(|r| &r.host.target_id == id))
            .map(|r| &r.host)
    })
}

/// What to call a remote target: its host's [`title`], else what its id says
/// ([`superba_core::target_title`]); never a raw app `hostId`.
pub fn target_name(cx: &Cx<'_>, id: &TargetId) -> String {
    match find(cx, id) {
        Some(host) => title(cx.tr, host),
        None => drawable(&cx.tr.text(&target_title(id))),
    }
}

/// Hover text naming the app's `hostId` of a host (`None` without one).
pub fn id_tip(tr: &Tr, host: &RemoteHost) -> Option<String> {
    host.host_id
        .as_deref()
        .map(|id| tr.f("hosts.host_id_tip", &[("id", &id)]))
}
