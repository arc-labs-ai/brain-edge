#!/usr/bin/env python3
"""Fail when the README and the source disagree about routes or env vars.

The README's `## API` section is the human-readable half of the same contract
`http_manifest.py` emits for the three SDK clients, and nothing connected the
two. It had drifted badly: 27 of 62 routes documented, and one paragraph
asserting that listing schema versions "is not exposed" while
`GET /v1/schemas` was served and brain-mcp's `list_schema_versions` tool was
calling it. An omission is a gap; a documented negative that is false is worse,
because a reader acts on it.

This compares both directions. A served route with no table row is
undocumented; a table row with no route describes something that does not
exist. Either fails.

The route set comes from `http_manifest.py`, so this agrees with what the SDKs
check themselves against rather than re-parsing the router a second way. The
two health probes live in `src/lib.rs` outside the manifest's scope and are
listed here.

The env table had the same problem and I did not notice while fixing the
routes: it documented 10 of 16 variables, and the six it omitted were the
credential-plane and key-encryption ones — `BRAIN_EDGE_PLANE`,
`BRAIN_EDGE_LLM_CREDENTIAL_FILE`, `BRAIN_EDGE_LLM_KEKS`,
`BRAIN_EDGE_LLM_KEK_CURRENT`, `BRAIN_EDGE_LLM_PROXY_TOKEN`. A route check that
leaves the security-relevant configuration undocumented is half a gate, so
both now run here.

    tools/check_readme_routes.py           # from the repo root
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
METHODS = ("GET", "POST", "PUT", "DELETE", "PATCH")

# Registered in `src/lib.rs`, which `http_manifest.py` does not scan: it reads
# the data-plane router. They are part of the documented surface regardless.
HEALTH = {("GET", "/health/live"), ("GET", "/health/ready")}


def documented() -> set[tuple[str, str]]:
    """Routes the README's tables declare, as (METHOD, path)."""
    out: set[tuple[str, str]] = set()
    pattern = re.compile(rf'^\|\s*`({"|".join(METHODS)})`\s*\|\s*`([^`]+)`')
    for line in (ROOT / "README.md").read_text().splitlines():
        m = pattern.match(line.strip())
        if m:
            out.add((m.group(1), m.group(2).strip()))
    return out


def served() -> set[tuple[str, str]]:
    """Routes the manifest generator finds in the router."""
    proc = subprocess.run(
        [sys.executable, str(ROOT / "tools" / "http_manifest.py"), str(ROOT / "src")],
        capture_output=True,
        text=True,
        check=True,
    )
    manifest = json.loads(proc.stdout)
    return {(r["method"].upper(), r["path"]) for r in manifest["routes"]} | HEALTH


# Read by the code but not configuration: a fixture for the "unset" path.
ENV_IGNORED = {"BRAIN_EDGE_TEST_DEFINITELY_UNSET_KEY"}
# Documented in the table but owned by the logging crate, not this source.
ENV_EXTERNAL = {"RUST_LOG"}


def documented_env() -> set[str]:
    """Variables the README's configuration table declares."""
    pattern = re.compile(r"^\|\s*`(BRAIN[A-Z_]*)`")
    out: set[str] = set()
    for line in (ROOT / "README.md").read_text().splitlines():
        m = pattern.match(line.strip())
        if m:
            out.add(m.group(1))
    return out


def read_env() -> set[str]:
    """Variables the source actually reads."""
    pattern = re.compile(r'"(BRAIN(?:_EDGE)?[A-Z_]*)"')
    out: set[str] = set()
    for path in (ROOT / "src").rglob("*.rs"):
        out.update(pattern.findall(path.read_text()))
    return out - ENV_IGNORED


def check_env() -> bool:
    """True when the env table and the source agree. Prints either way."""
    doc, live = documented_env() - ENV_EXTERNAL, read_env()
    undocumented = sorted(live - doc)
    phantom = sorted(doc - live)
    print(f"source reads {len(live)} env vars; README documents {len(doc)}")
    if undocumented:
        print(f"\n{len(undocumented)} env var(s) read with no README row:")
        for name in undocumented:
            print(f"    {name}")
    if phantom:
        print(f"\n{len(phantom)} README row(s) for env vars nothing reads:")
        for name in phantom:
            print(f"    {name}")
    return not (undocumented or phantom)


def main() -> int:
    doc, live = documented(), served()
    undocumented = sorted(live - doc)
    phantom = sorted(doc - live)

    print(f"router serves {len(live)} routes; README documents {len(doc)}")

    if undocumented:
        print(f"\n{len(undocumented)} route(s) served with no README row:")
        for method, path in undocumented:
            print(f"    {method:6} {path}")
    if phantom:
        print(f"\n{len(phantom)} README row(s) for routes that do not exist:")
        for method, path in phantom:
            print(f"    {method:6} {path}")

    routes_ok = not (undocumented or phantom)
    if not routes_ok:
        print("\nThe README's API tables and the router disagree.")
    else:
        print("README documents every served route, and no others.")

    print()
    env_ok = check_env()
    if env_ok:
        print("README documents every env var the source reads, and no others.")

    return 0 if (routes_ok and env_ok) else 1


if __name__ == "__main__":
    sys.exit(main())
