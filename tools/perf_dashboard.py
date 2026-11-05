#!/usr/bin/env python3
"""Render docs/perf/*.json into one static HTML page (no dependencies).

Two inputs are read:

- ffmpeg-compare-*.json: each decode clip's throughput ratio (Kinetix over ffmpeg,
  one thread; lower means Kinetix is further behind) charted across snapshots.
  A move of more than 10% between the last two snapshots is flagged.
- baseline-*.json: Criterion timings as reported, one column per snapshot.

Usage: python tools/perf_dashboard.py [docs/perf] [out.html]

The page is a report, not a gate: shared runners drift by 10-25%, so a flag is a
prompt to look, not a failure. See .github/workflows/bench.yml.
"""

import glob
import html
import json
import os
import sys

THRESHOLD = 0.10
PALETTE = ["#2563eb", "#dc2626", "#16a34a", "#9333ea", "#ea580c", "#0891b2", "#be185d", "#64748b"]


def load(directory, pattern):
    out = []
    for path in sorted(glob.glob(os.path.join(directory, pattern))):
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        out.append((os.path.basename(path), data))
    return out


def decode_series(compare):
    """clip -> list of (date, ratio) across snapshots, in file order."""
    series = {}
    for name, data in compare:
        date = str(data.get("date", name))
        for row in data.get("decode", []) or []:
            ratio = row.get("ratio_kinetix_over_ffmpeg_1thread")
            if ratio is None:
                continue
            label = f"{row.get('clip', '?')} [{row.get('codec', '?')}]"
            series.setdefault(label, []).append((date, float(ratio)))
    return series


def line_chart(points, width=520, height=160):
    """A small inline SVG line chart of (label, value) points."""
    if len(points) == 1:
        points = points * 2
    values = [v for _, v in points]
    lo, hi = min(values), max(values)
    span = (hi - lo) or max(hi, 1e-9)
    pad = 28
    step = (width - 2 * pad) / max(len(points) - 1, 1)
    coords = []
    for i, (_, v) in enumerate(points):
        x = pad + i * step
        y = height - pad - (v - lo) / span * (height - 2 * pad)
        coords.append((x, y))
    path = " ".join(f"{'M' if i == 0 else 'L'}{x:.1f},{y:.1f}" for i, (x, y) in enumerate(coords))
    dots = "".join(
        f'<circle cx="{x:.1f}" cy="{y:.1f}" r="3"><title>{html.escape(d)}: {v:.3f}</title></circle>'
        for (x, y), (d, v) in zip(coords, points)
    )
    return (
        f'<svg viewBox="0 0 {width} {height}" width="{width}" height="{height}" role="img">'
        f'<line x1="{pad}" y1="{height - pad}" x2="{width - pad}" y2="{height - pad}" stroke="#94a3b8"/>'
        f'<text x="2" y="{pad}" font-size="10">{hi:.2f}</text>'
        f'<text x="2" y="{height - pad}" font-size="10">{lo:.2f}</text>'
        f'<path d="{path}" fill="none" stroke="{PALETTE[0]}" stroke-width="2"/>{dots}'
        f"</svg>"
    )


def flag(points):
    """The relative change between the last two snapshots, if it exceeds THRESHOLD."""
    if len(points) < 2:
        return None
    (_, before), (_, after) = points[-2], points[-1]
    if before == 0:
        return None
    change = (after - before) / before
    if abs(change) > THRESHOLD:
        return change
    return None


def baseline_table(baselines):
    """bench -> {snapshot label: value} across the baseline files."""
    rows = {}
    labels = []
    for name, data in baselines:
        label = str(data.get("label", name))
        labels.append(label)
        for crate, benches in (data.get("results") or {}).items():
            for bench, value in benches.items():
                rows.setdefault(f"{crate} :: {bench}", {})[label] = value
    return labels, rows


def render(directory):
    compare = load(directory, "ffmpeg-compare-*.json")
    baselines = load(directory, "baseline-*.json")
    parts = [
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>",
        "<meta name='viewport' content='width=device-width,initial-scale=1'>",
        "<title>TPT Kinetix performance</title>",
        "<style>body{font:14px/1.5 system-ui,sans-serif;margin:24px;color:#0f172a}"
        "table{border-collapse:collapse;margin:12px 0}td,th{border:1px solid #e2e8f0;padding:4px 8px;text-align:left;vertical-align:top}"
        ".flag{color:#b91c1c;font-weight:600}.card{display:inline-block;margin:8px;padding:8px;border:1px solid #e2e8f0;border-radius:8px}"
        "@media (prefers-color-scheme: dark){body{background:#0f172a;color:#e2e8f0}td,th,.card{border-color:#334155}}</style>",
        "</head><body><h1>TPT Kinetix performance</h1>",
        "<p>Generated from <code>docs/perf/*.json</code> by <code>tools/perf_dashboard.py</code>. "
        "Shared runners drift by 10-25%: a flag means look, not fail.</p>",
        "<h2>Decode throughput vs ffmpeg (one thread)</h2>",
        "<p>Ratio = Kinetix Mpixels/s over ffmpeg Mpixels/s. Lower is further behind.</p>",
    ]
    series = decode_series(compare)
    if not series:
        parts.append("<p>No decode comparisons found.</p>")
    for label, points in sorted(series.items()):
        change = flag(points)
        note = ""
        if change is not None:
            note = f" <span class='flag'>{change:+.0%} since the previous snapshot</span>"
        latest = points[-1][1]
        parts.append(
            f"<div class='card'><div><strong>{html.escape(label)}</strong>: "
            f"latest {latest:.3f}{note}</div>{line_chart(points)}"
            f"<div>{' · '.join(html.escape(d) for d, _ in points)}</div></div>"
        )

    labels, rows = baseline_table(baselines)
    parts.append("<h2>Criterion baselines</h2>")
    if not rows:
        parts.append("<p>No baseline snapshots found.</p>")
    else:
        parts.append("<table><tr><th>benchmark</th>" + "".join(f"<th>{html.escape(l)}</th>" for l in labels) + "</tr>")
        for bench in sorted(rows):
            cells = "".join(f"<td>{html.escape(str(rows[bench].get(l, '')))}</td>" for l in labels)
            parts.append(f"<tr><td>{html.escape(bench)}</td>{cells}</tr>")
        parts.append("</table>")
    parts.append("</body></html>\n")
    return "".join(parts)


def main(argv):
    directory = argv[1] if len(argv) > 1 else os.path.join("docs", "perf")
    out = argv[2] if len(argv) > 2 else os.path.join(directory, "dashboard.html")
    with open(out, "w", encoding="utf-8") as f:
        f.write(render(directory))
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
