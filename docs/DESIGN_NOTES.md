# SPM design notes

Rules every page follows. The layout gate (`tests/layout/`) checks the ones
marked *gated*.

## Tokens

- Card radius 12 px, control radius 8 px, page gap 18 px, button gap 12 px.
- One button height everywhere: the toolkit's 28 px (`ui::button`). 40 px is
  the top bar's controls only. *Gated* for service rows (`svc-button`).
- Help text sits behind an (i) or a tooltip, never as a paragraph on the page.
  Every tooltip is the toolkit's or the (i)'s `InfoTip`, on the theme's
  popover tokens. *Gated* (`tooltips.sh`).

## Where buttons go

Action buttons always sit on the **right**. There are four slots, each with
one shared helper:

1. **Page actions** (Refresh, Add, Back up now): the right end of the page's
   title row.
2. **Card actions** that act on the whole card (Restore from…, Back up now):
   the right end of the card's header.
3. **Row actions**: the right end of the row, in one fixed-width column, so
   they line up on every row (Restore plus the three-dot menu,
   `ui::row_menu`). A row never ends in a bare chevron.
4. **Commit actions** (Apply, Save, Restart / Stop / Start, Next, Remove): the
   card or dialog foot, right end, with a divider above (`ui::card_actions`,
   `status::service_buttons`). *Gated* (`card-actions`).

Within any group:

- secondary actions on the left, the primary rightmost;
- Cancel or Back immediately left of the primary;
- a destructive action is never rightmost unless it is the dialog's purpose.
  In a menu it reads red (`RowMenuItem::Danger`) and still asks first;
- labels have no trailing "…" or "...": they say what happens ("Restore
  from file", "Pick another folder"), and a dialog that opens says so
  itself. *Gated* (`labels.py`).

## Colour

- Instance tiles: the header carries the instance tint, and the body is at
  most half of it (*gated*, `tint.py`). The top bar's tint is as faint in
  dark as in light (*gated*).
- State is shown in state colours (green, amber, red, grey). It is never
  shown in the instance's accent colour (the switcher's dots, the card pills).
- The loading shimmer is at least as visible in dark as in light (*gated*).

## Remote pages

- A page on a server keeps what it read last and draws it at once when it
  opens again (the Licence card keeps its last reading per instance for the
  run, never on disk), refreshing behind it; values change in place. Only a
  first open shows the loading skeleton.
- One open connection per server (crate::remote::conn), the same on Linux
  and Windows (Windows' OpenSSH has no ControlMaster, so none is used): one
  long-lived `ssh` per added server, from the Platform Manager's start until
  it quits or the server is removed or edited, running the agent's
  `--job serve` (crate::remote::serve). It is started the way a job is
  (`agent_cmd`: sudo, the password on standard input, never on a command
  line; Windows: this Platform Manager's own agent, a console program, in
  the administrator's session).
  - Framing: one JSON object per line each way. The agent says hello first
    (`{"spm_serve":1,"root":…}`); a request is
    `{"id":n,"work":{"job":[…]}|{"sh":{"cmd":…,"root":…}}|{"ps":…},"stdin":…}`,
    the answer `{"id":n,"code":…,"out":…,"err":…}` (or `refused`: the caller
    makes its own ssh call). Each request runs on its own thread there, as
    its own process (the job code is unchanged), so independent reads go at
    once over the one connection and are answered in any order.
  - Shell commands run as the signed-in account (`runuser` from the root
    agent, with its home), or as root for what used sudo before.
  - Keep-alives every 5 s (dropped within 15 s). A supervisor thread
    reconnects at once when it drops, then after 1, 2, 4 … 15 s while the
    server does not answer; a request made while it is down starts the next
    attempt at once and waits for it, so a page says the connection is lost
    only when reconnecting failed (as before). A request on a connection that
    drops fails; it is not sent again (it may have run).
  - An agent without serve mode (an older one answers with its usage text),
    none set up, or sudo refusing: one ssh call per request, as before (the
    licence request inline, `--request-b64`); asked again after 10 minutes,
    a new sudo password, or Set up. `SPM_NO_SERVE=1` forces that path.
  - After Set up / Update agent replaced the agent, its connection is
    closed (it finishes the requests on it); the next request starts the new
    one.
  - Still separate ssh calls: following a job's progress on Linux (a
    stream, `tail -F`), the Files page's sftp session (its own long-lived
    connection already), file copies and backup uploads/downloads (sftp,
    server to server), the first connection (detect), and deploy's sudo
    check and copy.
  - The built-in terminal (crate::terminal): one `ssh -tt` per open
    terminal, in a pseudo terminal of this computer, with the same key or
    kept password (askpass, never on a command line) and known_hosts. It
    writes nothing on the server: SPM's prompt is set for the session only
    (bash `--rcfile` from a here-document, PowerShell's `prompt` function),
    and a prompt theme the account uses is kept.
