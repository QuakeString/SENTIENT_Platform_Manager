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
- Every ssh call costs a handshake (several round trips): one call per
  request (a licence request goes inline, `--request-b64`), independent
  reads at once.
- Follow-up after 0.9.0: one ssh connection per server (OpenSSH
  ControlMaster/ControlPersist on Linux and macOS). Windows' OpenSSH client
  has no ControlMaster, so Windows needs another answer (a long-lived agent
  session over one ssh channel, or the agent listening on a forwarded port).
