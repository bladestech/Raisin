"""Generate src/devices_table.rs from an OpenRazer checkout.

OpenRazer's files are only read and parsed as text/AST; nothing from them is
imported or executed. Run from the repo root:

    git clone --depth 1 https://github.com/openrazer/openrazer <dir>
    python -I tools/openrazer_devices.py <dir> src/devices_table.rs docs/devices.md
"""
import ast
import json
import re
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1])
out = Path(sys.argv[2])
drv = root / "driver"

# ---- header: define -> PID ----
hdr = (drv / "razermouse_driver.h").read_text(encoding="utf-8", errors="replace")
pid_of = {m.group(1): int(m.group(2), 16)
          for m in re.finditer(r"#define\s+(USB_DEVICE_ID_RAZER_\w+)\s+(0x[0-9A-Fa-f]+)", hdr)}
waits = {m.group(1): int(m.group(2))
         for m in re.finditer(r"#define\s+(RAZER_\w*WAIT\w*)\s+(\d+)", hdr)}

src = (drv / "razermouse_driver.c").read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n")


def function_body(name_regex):
    """Return {name: body} for C function definitions matching the regex."""
    bodies = {}
    for m in re.finditer(r"\n(?:static\s+)?[\w\s\*]+?\b(" + name_regex + r")\s*\([^;{]*\)\s*\{", src):
        start = m.end() - 1
        depth = 0
        for i in range(start, len(src)):
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
                if depth == 0:
                    bodies[m.group(1)] = src[start:i + 1]
                    break
    return bodies


def case_groups(body):
    """Split a switch body into (labels, block_text) groups; 'default' included."""
    groups = []
    # Tokens: case labels / default, followed by statements until the next label run.
    pattern = re.compile(r"((?:\s*(?:case\s+\w+\s*:|default\s*:))+)")
    parts = pattern.split(body)
    # parts: [pre, labels, block, labels, block, ...]
    for k in range(1, len(parts) - 1, 2):
        labels = re.findall(r"case\s+(\w+)\s*:|(default)\s*:", parts[k])
        names = [a or b for a, b in labels]
        groups.append((names, parts[k + 1]))
    return groups


def per_pid(body, extract):
    """Map PID -> extract(block) for every case group in body."""
    result = {}
    default = None
    for names, block in case_groups(body):
        val = extract(block)
        if val is None:
            continue
        for n in names:
            if n == "default":
                default = val
            elif n in pid_of:
                result.setdefault(pid_of[n], val)
    return result, default


def tid(block):
    m = re.search(r"transaction_id\.id\s*=\s*(0x[0-9A-Fa-f]+)", block)
    return int(m.group(1), 16) if m else None


def calls(block):
    found = re.findall(r"\b(razer_chroma_\w+|razer_\w+_misc_\w+)\s*\(", block)
    return sorted(set(found)) or None


attrs = {}
for name, body in function_body(r"razer_attr_(?:read|write)_\w+").items():
    t, tdef = per_pid(body, tid)
    c, cdef = per_pid(body, calls)
    attrs[name] = {"tid": t, "tid_default": tdef, "calls": c, "calls_default": cdef}

# Receiver wait per PID (razer_get_report).
gr = function_body(r"razer_get_report")
wait_by_pid = {}
wait_default = None
if gr:
    def wait_of(block):
        m = re.search(r"razer_get_usb_response\([^;]*?(RAZER_\w*WAIT\w*)", block)
        return waits.get(m.group(1)) if m else None
    wait_by_pid, wait_default = per_pid(next(iter(gr.values())), wait_of)

# ---- daemon: classes ----
py = (root / "daemon" / "openrazer_daemon" / "hardware" / "mouse.py").read_text(encoding="utf-8")
tree = ast.parse(py)
classes = {}


def literal(node, scope):
    """Evaluate simple class attribute expressions: literals, list concat, Parent.ATTR."""
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        a, b = literal(node.left, scope), literal(node.right, scope)
        return a + b if a is not None and b is not None else None
    if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
        parent = classes.get(node.value.id)
        return resolve(parent, node.attr) if parent else None
    try:
        return ast.literal_eval(node)
    except Exception:
        return None


def resolve(cls, attr):
    while cls:
        if attr in cls["attrs"]:
            return cls["attrs"][attr]
        cls = classes.get(cls["bases"][0]) if cls["bases"] else None
    return None


for node in tree.body:
    if not isinstance(node, ast.ClassDef):
        continue
    entry = {"bases": [b.id for b in node.bases if isinstance(b, ast.Name)], "attrs": {},
             "doc": ast.get_docstring(node) or ""}
    classes[node.name] = entry
    for stmt in node.body:
        if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0], ast.Name):
            val = literal(stmt.value, entry)
            if val is not None:
                entry["attrs"][stmt.targets[0].id] = val

devices = []
for cname, cls in classes.items():
    pid = resolve(cls, "USB_PID")
    if not isinstance(pid, int) or cname.startswith("_"):
        continue
    doc = cls["doc"]
    m = re.search(r"Class for the (Razer .+?)\s*$", doc, re.M)
    name = m.group(1).strip() if m else cname
    methods = resolve(cls, "METHODS") or []
    pid_name = next((k for k, v in pid_of.items() if v == pid), None)

    def attr_info(a):
        info = attrs.get(a)
        if not info:
            return None, None
        t = info["tid"].get(pid, info["tid_default"])
        c = info["calls"].get(pid, info["calls_default"])
        return t, c

    dpi_tid, dpi_calls = attr_info("razer_attr_write_dpi")
    poll_tid, poll_calls = attr_info("razer_attr_write_poll_rate")
    batt_tid, _ = attr_info("razer_attr_read_charge_level")
    stages_tid, stages_calls = attr_info("razer_attr_write_dpi_stages")
    scroll_tid, _ = attr_info("razer_attr_write_scroll_mode")
    devices.append({
        "class": cname,
        "name": name,
        "pid": pid,
        "pid_define": pid_name,
        "dpi_max": resolve(cls, "DPI_MAX"),
        "poll_rates": resolve(cls, "POLL_RATES"),
        "available_dpi": resolve(cls, "AVAILABLE_DPI"),
        "methods": sorted(set(methods)),
        "tid": {"dpi": dpi_tid, "poll": poll_tid, "battery": batt_tid, "stages": stages_tid, "scroll": scroll_tid},
        "calls": {"dpi": dpi_calls, "poll": poll_calls, "stages": stages_calls},
        "wait_us": wait_by_pid.get(pid, wait_default),
    })

devices.sort(key=lambda d: d["pid"])

if out.suffix == ".json":
    out.write_text(json.dumps({"waits": waits, "wait_default": wait_default, "devices": devices}, indent=1))
    print(f"{len(devices)} mice -> {out}")
    sys.exit(0)

# ---- Rust table ----

# Hyper-polling mice whose second poll-rate request uses transaction ID 0xFF.
POLL_TID2_FF = {"USB_DEVICE_ID_RAZER_VIPER_8K", "USB_DEVICE_ID_RAZER_HYPERPOLLING_WIRELESS_DONGLE",
                "USB_DEVICE_ID_RAZER_DEATHADDER_V3_PRO_WIRELESS_ALT"}
STANDARD_RATES = {125, 500, 1000}  # all the standard 00/05 command can encode
HYPER_RATES = [125, 500, 1000, 2000, 4000, 8000]


def rust_row(d):
    m = set(d["methods"])
    calls_dpi = set(d["calls"]["dpi"] or [])
    calls_poll = " ".join(d["calls"]["poll"] or [])

    if "set_dpi_xy" in m and "razer_chroma_misc_set_dpi_xy" in calls_dpi:
        dpi = "Xy"
    elif "set_dpi_xy_byte" in m and "razer_chroma_misc_set_dpi_xy_byte" in calls_dpi:
        dpi = "Byte"
    else:
        dpi = "None"

    rates = d["poll_rates"]
    if "set_poll_rate" in m and "polling_rate2" in calls_poll:
        poll = "Hyper"
        rates = rates or HYPER_RATES
    elif "set_poll_rate" in m and "set_polling_rate" in calls_poll:
        poll = "Standard"
        rates = [r for r in (rates or sorted(STANDARD_RATES)) if r in STANDARD_RATES]
    else:
        poll = "None"
        rates = []

    t = d["tid"]
    base = t["dpi"] or t["poll"] or t["battery"] or 0xFF
    tids = (base, t["poll"] or base, t["battery"] or base, t["stages"] or base, t["scroll"] or base)
    has = {
        "stages": dpi == "Xy" and "set_dpi_stages" in m,
        "battery": "get_battery" in m,
        "charging": "is_charging" in m,
        "idle": "set_idle_time" in m,
        "low_battery": "set_low_battery_threshold" in m,
        "scroll_mode": "set_scroll_mode" in m,
        "scroll_accel": "set_scroll_acceleration" in m,
        "smart_reel": "set_scroll_smart_reel" in m,
    }
    # Mice that only accept a fixed list of DPI values: not supported yet.
    if d["available_dpi"]:
        dpi = "None"
        has["stages"] = False
    if dpi == "None" and poll == "None" and not has["battery"]:
        return None  # nothing Raisin can drive (e.g. pre-protocol mice)
    dpi_max = d["dpi_max"] or (6750 if dpi == "Byte" else 0)
    if dpi != "None" and not dpi_max:
        dpi = "None"
    tid2 = "Some(0xFF)" if d["pid_define"] in POLL_TID2_FF else "None"
    name = d["name"].replace('"', "'")
    b = {k: str(v).lower() for k, v in has.items()}
    return (
        f"    Device {{ pid: 0x{d['pid']:04X}, name: \"{name}\", dpi: DpiCmd::{dpi}, dpi_max: {dpi_max}, "
        f"poll: PollCmd::{poll}, poll_rates: &{json.dumps(rates)}, stages: {b['stages']}, "
        f"battery: {b['battery']}, charging: {b['charging']}, idle: {b['idle']}, low_battery: {b['low_battery']}, "
        f"scroll_mode: {b['scroll_mode']}, scroll_accel: {b['scroll_accel']}, smart_reel: {b['smart_reel']}, "
        f"tid: Tids {{ dpi: 0x{tids[0]:02X}, poll: 0x{tids[1]:02X}, power: 0x{tids[2]:02X}, "
        f"stages: 0x{tids[3]:02X}, scroll: 0x{tids[4]:02X} }}, "
        f"poll_tid2: {tid2}, wait_us: {d['wait_us'] or 600} }},"
    )


kept = [d for d in devices if rust_row(d)]
rows = [rust_row(d) for d in kept]
try:
    rev = subprocess.run(["git", "-C", str(root), "log", "-1", "--format=%h %cs"],
                         capture_output=True, text=True, check=True).stdout.strip()
except Exception:
    rev = "unknown revision"
header = (
    "// @generated by tools/openrazer_devices.py from OpenRazer " + rev + ".\n"
    "// Do not edit by hand: re-run the tool instead (see the tool's docstring).\n"
    "// Device facts (USB IDs, limits, command variants) come from OpenRazer\n"
    "// (https://github.com/openrazer/openrazer).\n\n"
    "use super::{Device, DpiCmd, PollCmd, Tids};\n\n"
    "#[rustfmt::skip]\n"
    "pub static DEVICES: &[Device] = &[\n"
)
out.write_text(header + "\n".join(rows) + "\n];\n", encoding="utf-8", newline="\n")
print(f"{len(rows)} of {len(devices)} mice -> {out}")

# ---- optional Markdown list ----
TESTED = {0x00B8}  # keep in sync with TESTED in src/devices.rs
if len(sys.argv) > 3:
    md = Path(sys.argv[3])
    lines = [
        "# Supported mice",
        "",
        f"Raisin knows {len(kept)} Razer mice. Their details come from [OpenRazer](https://github.com/openrazer/openrazer) "
        f"(revision {rev}); models marked **Tested** have been checked with Raisin on real hardware.",
        "",
        "Generated by `tools/openrazer_devices.py`; run `raisin devices` to see the same list.",
        "",
        "| Mouse | USB ID | Max DPI | Polling (Hz) | Battery | DPI stages | Scroll wheel | Status |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for d in kept:
        row = rust_row(d)
        rates = re.search(r"poll_rates: &\[([^\]]*)\]", row).group(1).replace(" ", "")
        max_dpi = re.search(r"dpi_max: (\d+)", row).group(1)
        dpi_none = "DpiCmd::None" in row
        yes = lambda key: "yes" if f"{key}: true" in row else ""
        lines.append(
            f"| {d['name']} | `1532:{d['pid']:04X}` | {'' if dpi_none else max_dpi} | {rates.replace(',', ', ')} "
            f"| {yes('battery')} | {yes('stages')} | {yes('scroll_mode')} | {'**Tested**' if d['pid'] in TESTED else ''} |"
        )
    md.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"-> {md}")
