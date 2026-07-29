#!/usr/bin/env python3
"""Emit brain-edge's HTTP contract as a machine-readable manifest.

The edge's JSON surface is a contract with three SDK HTTP clients, and until
now nothing connected the two sides — the DTOs here and the clients' types were
independent hand transcriptions of the same JSON, with no artifact in between.
That is the exact setup that produced `session_filter` and `RetrieverWire` on
the wire side, and it has already produced one instance here (`whoami` was
documented as returning `agent_id`; it returns `space_id`).

This emits what the wire side gets from `protocol.json`: every route with its
method, its request body / query type, its response type, and every DTO's
fields with their shapes and serde attributes. The SDKs check themselves
against the vendored copy.

Parsing Rust with regexes is normally a bad idea; it is workable here for the
same reason it is workable for the wire types — the DTOs are deliberately plain
`#[derive(Serialize/Deserialize)]` structs with a closed set of serde
attributes. Anything the parser cannot read lands in `unparsed` rather than
being dropped, because a silent drop is the failure this exists to prevent.

Usage:
    http_manifest.py <brain-edge-src-dir> > routes.json
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

# --- route table -----------------------------------------------------------

# `.route("/v1/x", get(handlers::a::b).post(handlers::c::d))` — one entry per
# HTTP method on the path. The argument list is scanned with balanced parens
# rather than a non-greedy regex: a lookahead to the next `.route(` runs past
# the closing paren on multi-method chains and attributes one path's handlers
# to another. It did exactly that, giving /v1/capabilities four methods.
ROUTE_START = re.compile(r'\.route\(\s*"([^"]+)"\s*,')
METHOD_CALL = re.compile(r"\b(get|post|put|delete|patch)\s*\(\s*handlers::(\w+)::(\w+)")


def parse_routes(text: str) -> list[dict]:
    out: list[dict] = []
    for m in ROUTE_START.finditer(text):
        depth = 1  # we are inside `.route(` already
        i = m.end()
        while i < len(text) and depth:
            if text[i] == "(":
                depth += 1
            elif text[i] == ")":
                depth -= 1
            i += 1
        args = text[m.end() : i - 1]
        for verb, module, handler in METHOD_CALL.findall(args):
            out.append(
                {"method": verb.upper(), "path": m.group(1), "handler": f"{module}::{handler}"}
            )
    return out


# --- handler signatures ----------------------------------------------------

# `pub async fn name(... Json(body): Json<T> ...) -> Result<Json<R>, ApiError>`
HANDLER = re.compile(
    r"pub async fn (\w+)\s*\((?P<args>.*?)\)\s*->\s*Result<Json<(?P<resp>\w+)>", re.S
)
JSON_ARG = re.compile(r"Json\((?:\w+)\):\s*Json<(\w+)>")
QUERY_ARG = re.compile(r"Query\((?:\w+)\):\s*Query<(\w+)>")


def parse_handlers(text: str, module: str) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for m in HANDLER.finditer(text):
        args = m.group("args")
        body = JSON_ARG.search(args)
        query = QUERY_ARG.search(args)
        out[f"{module}::{m.group(1)}"] = {
            "request_body": body.group(1) if body else None,
            "query": query.group(1) if query else None,
            "response": m.group("resp"),
        }
    return out


# --- DTOs ------------------------------------------------------------------

SERDE_ATTR = re.compile(r"#\[serde\((.*?)\)\]", re.S)
FIELD = re.compile(r"^\s*pub\s+(\w+)\s*:\s*(.+?),?\s*$")


def strip_line_comments(text: str) -> str:
    out = []
    for line in text.split("\n"):
        in_str = escaped = False
        cut = len(line)
        for i, c in enumerate(line):
            if escaped:
                escaped = False
                continue
            if c == "\\":
                escaped = True
                continue
            if c == '"':
                in_str = not in_str
                continue
            if not in_str and c == "/" and i + 1 < len(line) and line[i + 1] == "/":
                cut = i
                break
        out.append(line[:cut].rstrip())
    return "\n".join(out)


def serde_attrs(attr_text: str) -> dict:
    found: dict = {}
    for m in SERDE_ATTR.finditer(attr_text):
        for part in re.split(r",(?![^()]*\))", m.group(1)):
            part = part.strip()
            if not part:
                continue
            key, _, value = part.partition("=")
            key, value = key.strip(), value.strip().strip('"')
            if key in ("default", "skip", "skip_serializing_if", "rename", "alias", "flatten"):
                found[key] = value or True
    return found


def shape(ty: str) -> str:
    """Reduce a Rust type to the JSON shape a client must produce or expect."""
    ty = re.sub(r"\s+", " ", ty).strip().rstrip(",")
    ty = re.sub(r"\b[A-Za-z_][A-Za-z0-9_]*::", "", ty)
    m = re.match(r"^Option<(.+)>$", ty)
    if m:
        return f"opt({shape(m.group(1))})"
    m = re.match(r"^Vec<(.+)>$", ty)
    if m:
        return f"list({shape(m.group(1))})"
    if ty in ("String", "&str", "&'static str"):
        return "string"
    if ty == "bool":
        return "bool"
    if re.match(r"^[ui]\d+$", ty):
        return "int"
    if re.match(r"^f\d+$", ty):
        return "float"
    return ty  # a named DTO; resolved by the reader


def parse_dtos(text: str) -> tuple[dict, list]:
    text = strip_line_comments(text)
    dtos: dict = {}
    unparsed: list = []
    for m in re.finditer(r"pub struct (\w+)\s*\{", text):
        name = m.group(1)
        brace = text.index("{", m.end() - 1)
        depth = 0
        for j in range(brace, len(text)):
            if text[j] == "{":
                depth += 1
            elif text[j] == "}":
                depth -= 1
                if depth == 0:
                    break
        else:
            unparsed.append(f"struct {name}: unbalanced braces")
            continue

        fields = []
        pending: list[str] = []
        buf = ""
        angle = 0
        for raw in text[brace + 1 : j].split("\n"):
            line = raw.strip()
            if not line:
                continue
            if line.startswith("#["):
                pending.append(line)
                continue
            buf = (buf + " " + line).strip() if buf else line
            angle += buf.count("<") - buf.count(">")
            if angle > 0:
                continue
            fm = FIELD.match(buf)
            if fm:
                fields.append(
                    {
                        "name": fm.group(1),
                        "shape": shape(fm.group(2)),
                        "serde": serde_attrs(" ".join(pending)),
                    }
                )
            elif buf not in ("", "}"):
                unparsed.append(f"struct {name}: {buf}")
            pending, buf = [], ""
        dtos[name] = {"fields": fields}
    return dtos, unparsed


def collect(src: Path) -> dict:
    routes = parse_routes((src / "app" / "mod.rs").read_text())

    handlers: dict = {}
    for f in sorted((src / "app" / "handlers").glob("*.rs")):
        if f.stem == "mod":
            continue
        handlers.update(parse_handlers(f.read_text(), f.stem))

    dtos: dict = {}
    unparsed: list = []
    for f in sorted((src / "dto").glob("*.rs")):
        d, u = parse_dtos(f.read_text())
        dtos.update(d)
        unparsed += [f"{f.name}: {x}" for x in u]

    for r in routes:
        sig = handlers.get(r["handler"])
        if sig is None:
            unparsed.append(f"route {r['method']} {r['path']}: handler {r['handler']} not found")
            continue
        r.update(sig)

    return {
        "routes": sorted(routes, key=lambda r: (r["path"], r["method"])),
        "dtos": dtos,
        "unparsed": unparsed,
    }


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    print(json.dumps(collect(Path(sys.argv[1]).expanduser()), indent=1, sort_keys=True))
