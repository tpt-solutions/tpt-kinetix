#!/usr/bin/env python3
"""Generate Rust default-CDF consts for tpt-kinetix-av1 from the AV1 spec
``10.additional.tables.md`` (and ``08.decoding.process.md`` for the
inter-prediction subpel filter kernels).

The spec publishes every default CDF as a C-style initializer list fenced in a
``~~~~~ c`` block.  We locate each named table, extract the balanced-brace
block, and recursively parse it into nested integer lists, then emit a Rust
``pub static`` array whose shape is inferred from the nesting.

These tables are the *exact* default probabilities the AV1 arithmetic decoder
must start from (identical to every conformant encoder), so they are
transcribed verbatim from the spec rather than approximated.

Usage::

    python scripts/gen_av1_cdf.py 10.additional.tables.md [out.rs] \
        [--extra 08.decoding.process.md]

Multiple input files may be supplied; the first positional argument is the
primary table file and ``--extra`` adds further spec files to search.
"""
import re
import sys

# Tables we need for intra keyframe decoding (Phases C/D scaffolding).
TARGETS = [
    "Default_Partition_W8_Cdf",
    "Default_Partition_W16_Cdf",
    "Default_Partition_W32_Cdf",
    "Default_Partition_W64_Cdf",
    "Default_Partition_W128_Cdf",
    "Default_Intra_Frame_Y_Mode_Cdf",
    "Default_Y_Mode_Cdf",
    "Default_Uv_Mode_Cfl_Not_Allowed_Cdf",
    "Default_Uv_Mode_Cfl_Allowed_Cdf",
    "Default_Tx_8x8_Cdf",
    "Default_Tx_16x16_Cdf",
    "Default_Tx_32x32_Cdf",
    "Default_Tx_64x64_Cdf",
    "Default_Txfm_Split_Cdf",
    "Default_Skip_Cdf",
    "Default_Angle_Delta_Cdf",
    "Default_Interp_Filter_Cdf",
    "Default_Segment_Id_Cdf",
    # AV1 Phase E — inter prediction: inter mode / reference frame syntax.
    "Default_Is_Inter_Cdf",
    "Default_Skip_Mode_Cdf",
    "Default_Comp_Mode_Cdf",
    "Default_Comp_Ref_Type_Cdf",
    "Default_Uni_Comp_Ref_Cdf",
    "Default_Comp_Ref_Cdf",
    "Default_Comp_Bwd_Ref_Cdf",
    "Default_Single_Ref_Cdf",
    "Default_Compound_Mode_Cdf",
    "Default_New_Mv_Cdf",
    "Default_Zero_Mv_Cdf",
    "Default_Ref_Mv_Cdf",
    "Default_Drl_Mode_Cdf",
    "Default_Motion_Mode_Cdf",
    "Default_Use_Obmc_Cdf",
    # AV1 Phase E — motion vector syntax (§5.11.31-5.11.32).
    "Default_Mv_Joint_Cdf",
    "Default_Mv_Sign_Cdf",
    "Default_Mv_Class_Cdf",
    "Default_Mv_Class0_Bit_Cdf",
    "Default_Mv_Class0_Fr_Cdf",
    "Default_Mv_Class0_Hp_Cdf",
    "Default_Mv_Bit_Cdf",
    "Default_Mv_Fr_Cdf",
    "Default_Mv_Hp_Cdf",
]

# Small non-CDF lookup tables. `Subpel_Filters` (AV1 §7.11.3.4) is the
# normative 6x16x8 set of inter-prediction interpolation kernels; its taps are
# signed, so it is emitted as `i32` rather than the unsigned CDF type.
LOOKUP_TARGETS = [
    "Subpel_Filters",
]


def find_blocks(text):
    """Return dict name -> raw brace content (string inside outermost {}).

    Only *definitions* of the form ``Name[ dim ]...[ dim ] = { ... }`` are
    matched.  Some target names (notably ``Subpel_Filters``) also appear as
    array *subscripts* inside spec pseudo-code, e.g.
    ``Subpel_Filters[ interpFilter ][ (p >> 6) & SUBPEL_MASK ][ t ] * ...``;
    requiring an ``=`` immediately after the bracket list skips those.
    """
    out = {}
    wanted = set(TARGETS) | set(LOOKUP_TARGETS)
    # Declaration: the name, one or more `[...]` dimensions, then `=`.
    decl = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)\s*((?:\[[^\]]*\]\s*)+)=")
    for m in decl.finditer(text):
        name = m.group(1)
        if name not in wanted or name in out:
            continue
        # find first '{' after the '='
        brace = text.find("{", m.end() - 1)
        if brace == -1:
            continue
        # match braces
        depth = 0
        i = brace
        while i < len(text):
            c = text[i]
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    out[name] = text[brace + 1 : i]
                    break
            i += 1
    return out


def parse_value(s, i):
    """Parse one value starting at index i. Returns (value, next_index).

    value is either an int or a list of values.  Scalars may be written as a
    product (the spec writes several default CDF probabilities as ``128*128``),
    which is evaluated here.
    """
    # skip whitespace and commas
    while i < len(s) and s[i] in " \t\r\n,":
        i += 1
    if i >= len(s):
        return None, i
    if s[i] == "{":
        lst = []
        i += 1
        while True:
            while i < len(s) and s[i] in " \t\r\n,":
                i += 1
            if i < len(s) and s[i] == "}":
                i += 1
                break
            v, i = parse_value(s, i)
            if v is None:
                break
            lst.append(v)
        return lst, i
    # integer, or a product of integers (`128*128`)
    m = re.match(r"-?\d+(?:\s*\*\s*-?\d+)*", s[i:])
    if not m:
        # skip a stray token (e.g. a comment start)
        j = i
        while j < len(s) and s[j] not in ",{} \t\r\n":
            j += 1
        return None, j
    value = 1
    for part in m.group(0).split("*"):
        value *= int(part.strip())
    return value, i + m.end()


def to_rust_type(node):
    if isinstance(node, int):
        return "u16"
    # list
    elem = node[0] if node else 0
    inner = to_rust_type(elem)
    return f"[{inner}; {len(node)}]"


def to_rust_literal(node):
    if isinstance(node, int):
        return str(node)
    parts = [to_rust_literal(x) for x in node]
    return "[" + ", ".join(parts) + "]"


def fmt_rust(name, node, is_lookup):
    rust_name = name.upper()
    if is_lookup:
        # Signed lookup tables (e.g. the subpel filter taps) need a signed
        # element type; CDF-shaped tables stay `u16`.
        elem = "i32" if min_int(node) < 0 else ("u8" if max_int(node) < 256 else "u16")
        ty = to_rust_type(node).replace("u16", elem)
        return f"pub static {rust_name}: {ty} = {to_rust_literal(node)};\n"
    ty = to_rust_type(node)
    return f"pub static {rust_name}: {ty} = {to_rust_literal(node)};\n"


def max_int(node):
    if isinstance(node, int):
        return node
    return max((max_int(x) for x in node), default=0)


def min_int(node):
    if isinstance(node, int):
        return node
    return min((min_int(x) for x in node), default=0)


def main():
    args = [a for a in sys.argv[1:]]
    extra = []
    while "--extra" in args:
        i = args.index("--extra")
        extra.append(args[i + 1])
        del args[i : i + 2]
    path = args[0] if args else "av1_tables.md"

    text = ""
    for p in [path] + extra:
        with open(p, "r", encoding="utf-8") as f:
            text += f.read() + "\n"
    blocks = find_blocks(text)

    lines = []
    lines.append("// @generated by scripts/gen_av1_cdf.py from AV1 spec 10.additional.tables.md\n")
    lines.append("// (Subpel_Filters from 08.decoding.process.md).\n")
    lines.append("// Exact default CDF / lookup tables. Do not edit by hand.\n")
    missing = []
    for name in TARGETS:
        if name not in blocks:
            missing.append(name)
            continue
        node, _ = parse_value("{" + blocks[name] + "}", 0)
        lines.append(fmt_rust(name, node, False))
    for name in LOOKUP_TARGETS:
        if name not in blocks:
            missing.append(name)
            continue
        node, _ = parse_value("{" + blocks[name] + "}", 0)
        lines.append(fmt_rust(name, node, True))
    if missing:
        sys.stderr.write("MISSING: " + ", ".join(missing) + "\n")
    out = "".join(lines)
    if len(args) > 1:
        with open(args[1], "w", encoding="utf-8") as f:
            f.write(out)
    else:
        sys.stdout.write(out)


if __name__ == "__main__":
    main()
