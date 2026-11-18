#!/bin/bash
# fetch_deps.sh -- download the Java libraries the launcher uses (Netty: the built-in LAN relay)
# into build/deps/, checked against pinned SHA-256 sums. Called by build_windows.sh; run it once
# by hand before compiling for tools/run_revived.sh. Prints the jar paths, one per line.
set -e
R="$(cd "$(dirname "$0")/.." && pwd)"
D=$R/build/deps
NETTY=4.1.140.Final
# artifact sha256
JARS="
netty-common 298e6c4ac1e01362c8be9db30728b8ab4c93e32fd5d916bbe15170df6321b97c
netty-buffer d79b3012f1cce4052cf18a89132a839019807e52c4db21bb5bc34e6e3a5248fc
netty-resolver 02513f495dcb4400b4b9df700c462b831c7efa3f2d0515c7ede496fb8852cc92
netty-transport bfb7a296c1dceaeee706e1330ecf1cb324b32badb68098f70b663717aacfe8b0
netty-codec 80d2ea6050ec628464c3beea6549a09544e4c19fd7579543e1a658e1caae7e41
netty-handler ed825f76a081de170ce6224f5a1807846db86236f4943394a7e7a1a379c41bf9
netty-codec-http 8d8fc6746f2db2aad9339e7001a8c8b254ede3b47e0440cdef9df19af100d61f
"
# Windows' own curl uses the Windows certificate store (Git's curl may lack the CA bundle).
CURL=curl; [ -x /c/Windows/System32/curl.exe ] && CURL=/c/Windows/System32/curl.exe
mkdir -p "$D"
echo "$JARS" | while read -r a sum; do
  [ -n "$a" ] || continue
  f=$D/$a-$NETTY.jar
  if [ ! -f "$f" ] || [ "$(sha256sum "$f" | cut -d' ' -f1)" != "$sum" ]; then
    "$CURL" -sSfL -m 120 -o "$f.part" "https://repo1.maven.org/maven2/io/netty/$a/$NETTY/$a-$NETTY.jar"
    got=$(sha256sum "$f.part" | cut -d' ' -f1)
    if [ "$got" != "$sum" ]; then echo "checksum mismatch for $a: $got" >&2; rm -f "$f.part"; exit 1; fi
    mv "$f.part" "$f"
  fi
  echo "$f"
done
