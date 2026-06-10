#!/usr/bin/env python3
"""Patch source files with `field: None` lines for newly-optional API fields.

Runs `cargo check --workspace --message-format=json`, parses E0063 (missing
fields in struct initializer) diagnostics, and — only when every missing
field is optional in the committed OpenAPI spec — inserts the field with a
literal `None` into the struct literal. Loops up to MAX_ITERS times in case
patches surface more errors.

Always exits 0. A markdown summary is written to --summary-file describing
patches applied and any errors that remain. The caller decides whether to
fail the build based on whether the tree still compiles.
"""

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

MAX_ITERS = 3
SPEC_PATH = Path("src/testquorum-api/openapi.json")

E0063_MSG_RE = re.compile(r"^missing fields? (.+) in initializer of `([^`]+)`$")
BACKTICK_FIELD_RE = re.compile(r"`([^`]+)`")


def run_cargo_check():
    """Return the list of compiler-message diagnostics from cargo check."""
    proc = subprocess.run(
        ["cargo", "check", "--workspace", "--message-format=json"],
        capture_output=True,
        text=True,
    )
    diagnostics = []
    for line in proc.stdout.splitlines():
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if obj.get("reason") != "compiler-message":
            continue
        msg = obj.get("message") or {}
        if msg.get("level") not in ("error", "error: internal compiler error"):
            continue
        diagnostics.append(msg)
    return diagnostics


def parse_e0063(msg):
    """Return (struct_name, [missing_fields], primary_span) or None."""
    m = E0063_MSG_RE.match(msg["message"])
    if not m:
        return None
    field_list_str, struct_name = m.group(1), m.group(2)
    missing = BACKTICK_FIELD_RE.findall(field_list_str)
    primary = next((s for s in msg["spans"] if s.get("is_primary")), None)
    if not primary:
        return None
    return struct_name, missing, primary


_WS = b" \t\r\n"


def find_matching_brace(data, start):
    """Given byte offset `start` just after a type name in the file's bytes,
    find the matching `}` of the struct literal that follows. Returns the
    byte offset of the closing `}`, or None if we can't parse it cleanly.

    rustc spans use byte offsets, so we operate on bytes throughout — the
    crate has multi-byte UTF-8 chars (em-dashes) in comments that would
    otherwise shift indices."""
    i = start
    n = len(data)
    while i < n and data[i:i + 1] in (b" ", b"\t", b"\r", b"\n"):
        i += 1
    if i >= n or data[i:i + 1] != b"{":
        return None
    depth = 0
    while i < n:
        c = data[i:i + 1]
        if c == b"{":
            depth += 1
        elif c == b"}":
            depth -= 1
            if depth == 0:
                return i
        elif c == b'"':
            i += 1
            while i < n:
                if data[i:i + 1] == b"\\":
                    i += 2
                    continue
                if data[i:i + 1] == b'"':
                    break
                i += 1
        elif c == b"'":
            # Char literal or lifetime. Lifetimes (`'a`) have no closing `'`
            # and shouldn't appear inside a struct-literal value position, but
            # be defensive: only treat as a char literal if we can see a
            # closing `'` within 4 bytes.
            j = i + 1
            if j < n and data[j:j + 1] == b"\\":
                k = j + 2
                while k < n and k < j + 6 and data[k:k + 1] != b"'":
                    k += 1
                if k < n and data[k:k + 1] == b"'":
                    i = k
            elif j + 1 < n and data[j + 1:j + 2] == b"'":
                i = j + 1
        elif c == b"/" and data[i + 1:i + 2] == b"/":
            while i < n and data[i:i + 1] != b"\n":
                i += 1
            continue
        elif c == b"/" and data[i + 1:i + 2] == b"*":
            i += 2
            while i + 1 < n and not (
                data[i:i + 1] == b"*" and data[i + 1:i + 2] == b"/"
            ):
                i += 1
            i += 1
        i += 1
    return None


def is_all_optional_in_spec(spec, struct_name, missing):
    """Return (ok, reason). ok=True iff every missing field exists in the
    spec for `struct_name` and none are in `required`."""
    schemas = spec.get("components", {}).get("schemas", {})
    schema = schemas.get(struct_name)
    if not schema:
        return False, f"no schema named `{struct_name}` in openapi.json"
    properties = schema.get("properties", {})
    required = set(schema.get("required", []))
    for f in missing:
        if f not in properties:
            return False, f"field `{f}` not in spec for `{struct_name}`"
        if f in required:
            return False, f"field `{f}` is required in spec for `{struct_name}`"
    return True, ""


def apply_patches(file_path, patches):
    """patches: list of (struct_close_byte, [field_names]). Applied bottom-up.
    Operates on bytes to keep offsets aligned with rustc spans."""
    data = Path(file_path).read_bytes()
    patches = sorted(patches, key=lambda p: p[0], reverse=True)
    for close_byte, fields in patches:
        line_start = data.rfind(b"\n", 0, close_byte) + 1
        closing_line_prefix = data[line_start:close_byte]
        if closing_line_prefix.strip() != b"":
            return False, f"struct literal at byte {close_byte} is single-line"
        base_indent = closing_line_prefix
        field_indent = base_indent + b"    "
        insertion = b"".join(
            field_indent + f.encode("utf-8") + b": None,\n" for f in fields
        )
        data = data[:line_start] + insertion + data[line_start:]
    Path(file_path).write_bytes(data)
    return True, ""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--summary-file", required=True, type=Path)
    args = parser.parse_args()

    if not SPEC_PATH.exists():
        print(f"openapi spec not found at {SPEC_PATH}", file=sys.stderr)
        args.summary_file.write_text(
            f"Patcher could not run: `{SPEC_PATH}` not found.\n"
        )
        return 0

    spec = json.loads(SPEC_PATH.read_text())
    summary_lines = []
    total_patched = 0

    for iteration in range(MAX_ITERS):
        diagnostics = run_cargo_check()
        e0063s = []
        for d in diagnostics:
            parsed = parse_e0063(d)
            if parsed:
                e0063s.append(parsed)
        if not e0063s:
            break

        by_file = defaultdict(list)
        for struct_name, missing, span in e0063s:
            file = span["file_name"]
            ok, reason = is_all_optional_in_spec(spec, struct_name, missing)
            if not ok:
                summary_lines.append(
                    f"- SKIP `{file}:{span['line_start']}` "
                    f"`{struct_name}` += {missing}: {reason}"
                )
                continue
            data = Path(file).read_bytes()
            close = find_matching_brace(data, span["byte_end"])
            if close is None:
                summary_lines.append(
                    f"- SKIP `{file}:{span['line_start']}` "
                    f"`{struct_name}`: could not locate closing `}}`"
                )
                continue
            by_file[file].append((close, missing))
            summary_lines.append(
                f"- patched `{file}:{span['line_start']}` "
                f"`{struct_name}` += {', '.join(f'`{f}`' for f in missing)}"
            )

        if not by_file:
            break

        for file, patches in by_file.items():
            ok, reason = apply_patches(file, patches)
            if not ok:
                summary_lines.append(f"- patch failed for `{file}`: {reason}")
            else:
                total_patched += len(patches)

    final = run_cargo_check()
    remaining_errors = []
    for d in final:
        if d.get("level") != "error":
            continue
        code = (d.get("code") or {}).get("code") or "???"
        primary = next((s for s in d.get("spans", []) if s.get("is_primary")), None)
        loc = (
            f"{primary['file_name']}:{primary['line_start']}" if primary else "<no span>"
        )
        first_line = d["message"].splitlines()[0]
        remaining_errors.append(f"  - `{code}` at `{loc}`: {first_line}")

    out = []
    if total_patched or summary_lines:
        out.append("### Auto-patched optional fields\n")
        out.append(
            f"Applied {total_patched} patch(es) for fields added to the API "
            "spec as optional. Review that `None` is the desired value for "
            "each new field.\n"
        )
        out.extend(summary_lines)
        out.append("")
    if remaining_errors:
        out.append(
            "### Build still broken after patching\n\n"
            "The patcher could not auto-fix every error. A human will need "
            "to land follow-up changes:\n"
        )
        out.extend(remaining_errors)
        out.append("")

    args.summary_file.write_text("\n".join(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
