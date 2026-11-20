# Translating the Platform Manager

`app/i18n/en.json` lists every on-screen text (English is the key).
`app/i18n/<code>.json` maps each to its translation. A missing text shows in
English. Regenerate the list with `python3 tools/i18n/scripts/catalogue.py`;
`split.py <code>` makes chunks of the texts a language still lacks;
`check.py <code>` merges them back and rejects broken ones.

## Rules for a translation

- Industrial software for plant engineers and IT staff: plain, short, the
  usual words of software UI in that language (as Windows / Android use them).
  Keep the tone: calm, direct, no exclamation marks.
- Placeholders `{0}`, `{1}`, `{name}` are filled in by the program: keep each
  one exactly once, unchanged; move them where the grammar needs them. `{{`
  and `}}` are literal braces.
- Keep `\n` line breaks where the English has them.
- Never translate: SENTIENT, Platform Manager (product name), INVENIA,
  PostgreSQL, ClickHouse, TimescaleDB, Docker, WSL, Windows, Linux, systemd,
  SSH, SFTP, HTTP(S), MQTT, CoAP, TLS, DTLS, gRPC, JSON, YAML, SHA-256, sudo,
  file names, paths, commands, environment variables (ALL_CAPS), versions,
  ports, units (GB, MB, KB, s, ms, %), keys and codes.
- Write digits as 0-9: the program shows numbers in the language's own digits
  where SENTIENT does.
- "tenant" is the SENTIENT customer account (never "renter"/"lodger");
  "instance" is one SENTIENT installation; "backup"/"restore" as in the OS.
- Use SENTIENT's own words for the same things: its UI translation
  `SENTIENT/ui-ngx/src/assets/locale/locale.constant-<code>.json`.
- Scripts: mni_IN Meitei Mayek, sat_IN Ol Chiki, dz_BT Tibetan (Dzongkha),
  ks_IN and sd_IN Perso-Arabic, ur_IN Urdu Nastaliq text in Arabic script,
  brx/doi/kok/mai/sa/ne/hi/mr Devanagari, pa_IN Gurmukhi, zh_TW Traditional,
  nl_BE Flemish usage, pt_BR Brazilian.
- Texts that are fragments (" · {0} free", ", released {0}") stay fragments,
  with the same leading/trailing spaces and punctuation role.
