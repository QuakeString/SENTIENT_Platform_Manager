#!/usr/bin/env bash
# Copyright © 2016-2026 The SENTIENT Authors
#
# The oldest glibc an ELF program can run on: the highest GLIBC_x.y[.z]
# version among its NON-WEAK undefined dynamic symbols. A weak reference
# (bind WEAK, " w " in objdump -T) resolves to null when the symbol is
# missing, so it does not raise the requirement and is left out.
#
#   scripts/elf-min-glibc.sh FILE...          highest version over all FILEs
#   scripts/elf-min-glibc.sh --prefix OPENSSL_ FILE...
#                                             same for another version prefix
#   scripts/elf-min-glibc.sh --self-test      check the parser and this host
#
# Non-ELF files (scripts) are skipped. Prints nothing when no FILE needs a
# versioned symbol of that prefix. Needs readelf (binutils).
#
# Sourcing this file defines elf_min_version PREFIX FILE... instead.

# Highest PREFIXx.y version on stdin, `readelf -W --dyn-syms` output.
# Columns: Num: Value Size Type Bind Vis Ndx Name[@VER (n)]
_elf_max_from_dynsyms() {
  local prefix=$1
  awk -v p="$prefix" '
    $7 == "UND" && $5 != "WEAK" {
      n = $8
      i = index(n, "@" p)
      if (i == 0) next
      v = substr(n, i + 1 + length(p))
      sub(/^@/, "", v)            # "@@" form
      sub(/[^0-9.].*$/, "", v)    # drop " (3)" and anything after
      if (v ~ /^[0-9]+(\.[0-9]+)*$/) print v
    }' | sort -V | tail -n 1
}

_elf_is_elf() {
  [ -f "$1" ] && [ "$(head -c 4 "$1" | od -An -c | tr -d ' ')" = "177ELF" ]
}

# elf_min_version PREFIX FILE... -> highest version, or empty
elf_min_version() {
  local prefix=$1; shift
  local f all=""
  for f in "$@"; do
    _elf_is_elf "$f" || continue
    all+=$(readelf -W --dyn-syms "$f" 2>/dev/null | _elf_max_from_dynsyms "$prefix")$'\n'
  done
  printf '%s' "$all" | sed '/^$/d' | sort -V | tail -n 1
}

_elf_self_test() {
  local fail=0 got
  # 1. Parser, on a fixed readelf excerpt: the WEAK 2.99 reference and the
  #    defined (non-UND) 2.50 symbol must not count; 2.34 is the answer.
  got=$(_elf_max_from_dynsyms GLIBC_ <<'EOF'
Symbol table '.dynsym' contains 6 entries:
   Num:    Value          Size Type    Bind   Vis      Ndx Name
     0: 0000000000000000     0 NOTYPE  LOCAL  DEFAULT  UND
     1: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND memcpy@GLIBC_2.14 (2)
     2: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND __libc_start_main@GLIBC_2.34 (3)
     3: 0000000000000000     0 FUNC    WEAK   DEFAULT  UND __cxa_thread_atexit_impl@GLIBC_2.99 (4)
     4: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND pthread_key_create@GLIBC_2.2.5 (5)
     5: 0000000000401000    10 FUNC    GLOBAL DEFAULT   14 local_def@@GLIBC_2.50
     6: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND SSL_new@OPENSSL_3.0.0 (6)
EOF
)
  if [ "$got" = "2.34" ]; then echo "ok   parser: 2.34 (weak 2.99 and defined 2.50 ignored)"
  else echo "FAIL parser: got '$got', want 2.34"; fail=1; fi
  got=$(printf '     6: 0 0 FUNC GLOBAL DEFAULT UND SSL_new@OPENSSL_3.0.0 (6)\n' | _elf_max_from_dynsyms OPENSSL_)
  if [ "$got" = "3.0.0" ]; then echo "ok   prefix OPENSSL_: 3.0.0"
  else echo "FAIL prefix OPENSSL_: got '$got', want 3.0.0"; fail=1; fi

  # 2. A real binary on this host: bash needs some GLIBC_ version, and never
  #    more than the glibc it is running on.
  local host target
  host=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}')
  target=$(command -v bash)
  got=$(elf_min_version GLIBC_ "$target")
  if [ -z "$got" ]; then echo "FAIL $target: no GLIBC_ version found"; fail=1
  elif [ -n "$host" ] && [ "$(printf '%s\n%s\n' "$got" "$host" | sort -V | tail -n 1)" != "$host" ]; then
    echo "FAIL $target: needs $got, above host glibc $host"; fail=1
  else echo "ok   $target: needs GLIBC_$got (host glibc ${host:-unknown})"; fi

  # 3. Non-ELF input is skipped.
  got=$(elf_min_version GLIBC_ "${BASH_SOURCE[0]}")
  if [ -z "$got" ]; then echo "ok   non-ELF skipped"
  else echo "FAIL non-ELF: got '$got'"; fail=1; fi
  return $fail
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  set -euo pipefail
  prefix=GLIBC_
  case "${1:-}" in
    --self-test) _elf_self_test; exit $? ;;
    --prefix) prefix=${2:?--prefix needs a value}; shift 2 ;;
    ""|-h|--help) sed -n '3,16p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
  esac
  [ $# -gt 0 ] || { echo "no files" >&2; exit 2; }
  command -v readelf >/dev/null || { echo "readelf not found (install binutils)" >&2; exit 2; }
  elf_min_version "$prefix" "$@"
fi
