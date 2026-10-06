#!/usr/bin/env python3
"""Mirror the backlog units and issues/ into Plane (one way: repo -> Plane).

The repository stays canonical. This script only computes *what* to push; the
`plane-sync` skill performs the Plane MCP calls and acknowledges each one.

  plane-sync.py export              all syncable items as JSON
  plane-sync.py plan                items whose content changed since the last ack
  plane-sync.py show <external_id>  one item's full payload (pushed one at a time)
  plane-sync.py ack <external_id> <plane_id>
  plane-sync.py selftest

State lives outside the repo (worktrees share it): $XDG_CACHE_HOME/ravel/plane-sync.jsonl.
It is append-only (last line per external_id wins) so parallel acks don't lose writes.
"""
import glob
import hashlib
import html
import json
import os
import re
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GH = "https://github.com/narusenia/ravel/blob/main/"
STATE = os.path.join(os.environ.get("XDG_CACHE_HOME", os.path.expanduser("~/.cache")), "ravel", "plane-sync.jsonl")
SEV = {"critical": "urgent", "high": "high", "medium": "medium", "low": "low"}
UNIT_STATE = {"✅": "Done", "❌": "Cancelled", "🟡": "Todo", "⬜": "Backlog", "❓": "Backlog"}


def inline(s):
    s = html.escape(s, quote=False)
    s = re.sub(r"`([^`]+)`", r"<code>\1</code>", s)
    s = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", s, flags=re.S)
    return re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r'<a href="\2">\1</a>', s)


def joined(lines):
    # Bold may span lines, so convert first and break lines after.
    return inline("\n".join(lines)).replace("\n", "<br>")


# ponytail: hand-rolled md->html (no markdown lib installed); nested lists flatten.
def md2html(md):
    out, para, i = [], [], 0
    lines = md.split("\n")

    def flush():
        if para:
            out.append("<p>" + joined(para) + "</p>")
            para.clear()

    while i < len(lines):
        l = lines[i]
        if l.startswith("```"):
            flush()
            j = i + 1
            while j < len(lines) and not lines[j].startswith("```"):
                j += 1
            out.append("<pre><code>" + html.escape("\n".join(lines[i + 1:j])) + "</code></pre>")
            i = j + 1
            continue
        m = re.match(r"^(#{1,6}) (.*)", l)
        if m:
            flush()
            n = min(len(m.group(1)) + 1, 6)
            out.append(f"<h{n}>{inline(m.group(2))}</h{n}>")
            i += 1
            continue
        if l.startswith("|"):
            flush()
            rows = []
            while i < len(lines) and lines[i].startswith("|"):
                cells = [c.strip() for c in lines[i].strip().strip("|").split("|")]
                if not all(re.fullmatch(r":?-+:?", c) for c in cells):
                    rows.append(cells)
                i += 1
            t = "<table>"
            for k, r in enumerate(rows):
                tag = "th" if k == 0 else "td"
                t += "<tr>" + "".join(f"<{tag}>{inline(c)}</{tag}>" for c in r) + "</tr>"
            out.append(t + "</table>")
            continue
        if l.startswith(">"):
            flush()
            q = []
            while i < len(lines) and lines[i].startswith(">"):
                q.append(lines[i].lstrip("> "))
                i += 1
            out.append("<blockquote><p>" + joined(q) + "</p></blockquote>")
            continue
        m = re.match(r"^\s*([-*]|\d+\.) (.*)", l)
        if m:
            flush()
            tag = "ol" if m.group(1)[0].isdigit() else "ul"
            items = []
            while i < len(lines):
                m2 = re.match(r"^\s*([-*]|\d+\.) (.*)", lines[i])
                if m2:
                    items.append(m2.group(2))
                elif lines[i].startswith("  ") and items:
                    items[-1] += " " + lines[i].strip()
                else:
                    break
                i += 1
            out.append(f"<{tag}>" + "".join(f"<li>{inline(x)}</li>" for x in items) + f"</{tag}>")
            continue
        if l.strip() in ("", "---"):
            flush()
        else:
            para.append(l)
        i += 1
    flush()
    return "\n".join(out)


def units():
    section, in_all = None, False
    for l in open(f"{REPO}/docs/implementation/backlog.md", encoding="utf-8").read().split("\n"):
        if l.startswith("## "):
            in_all = l.strip() == "## 全単位"
            continue
        if not in_all:
            continue
        if l.startswith("### "):
            # A plan moving to done/ must not move its units to a new module.
            section = l[4:].replace("`", "").replace("done/", "").strip()
            continue
        m = re.match(r"^\| ([A-Z0-9]+-[0-9]+[a-z]?) \| ([^|]*)\| ([^|]*)\| ([^|]*)\|", l)
        if not m:
            continue
        uid, st, name, dep = (x.strip() for x in m.groups())
        mark = next((k for k in UNIT_STATE if k in st), None)
        if not mark:  # "→" (moved) and "—" rows are not units of their own
            continue
        yield dict(
            external_id=uid, name=f"{uid} {name.replace('`', '')}", state=UNIT_STATE[mark],
            labels=["unit"] + (["判断待ち"] if mark == "❓" else []), priority="none", module=section,
            description_html=f"<p><strong>状態</strong>: {mark}</p><p><strong>依存</strong>: {inline(dep) or '—'}</p>"
                             f"<p><strong>節</strong>: {inline(section)}</p>"
                             f'<p>正本: <a href="{GH}docs/implementation/backlog.md">docs/implementation/backlog.md</a>（設計は各計画書）</p>')


ITEM = re.compile(r"^(?:#{2,4} |\*\*)([A-Z]+(?:-[A-Z]+)?-\d+) \| (\w+(?: / \w+)*) \| (.*?)(?:\*\*)?$")


def issues():
    seen = set()

    def item(iid, sev, kind, title, closed, rel, body):
        # The repo has reused a few IDs; the later file in glob order gets "~2".
        ext = iid if iid not in seen else iid + "~2"
        seen.add(iid)
        return dict(
            external_id=ext, name=f"[{iid}] {title.replace('`', '')}", state="Done" if closed else "Todo",
            labels=["issue"] + kind.split(" / "), priority=SEV[sev], module=None,
            description_html=f'<p>正本: <a href="{GH}{rel}">{rel}</a></p>\n' + md2html(body.strip()))

    for path in sorted(glob.glob(f"{REPO}/issues/*/*.md")):
        if os.path.basename(path) == "README.md":
            continue
        md = open(path, encoding="utf-8").read()
        closed, rel = "/closed/" in path, os.path.relpath(path, REPO)
        first = md.split("\n", 1)[0]
        if re.match(r"^# \[?(CRIT|HIGH)-", first):  # one finding per file
            m = re.match(r"^# \[?([A-Z]+-\d+)\]? ?(?:\| (\w+(?: / \w+)*) \| )?(.*)", first)
            sev = re.search(r"^\| 深刻度 \| (\w+) \|", md, re.M)
            kind = m.group(2) or re.search(r"^\| 種別 \| (\w+) \|", md, re.M).group(1)
            sev = sev.group(1) if sev else ("critical" if m.group(1).startswith("CRIT") else "high")
            yield item(m.group(1), sev, kind, m.group(3), closed, rel, md.split("\n", 1)[1])
            continue
        sev = "low" if "low" in os.path.basename(path) or "/low/" in path else "medium"
        cur = None  # medium / low files hold many findings each
        for l in md.split("\n"):
            mm = ITEM.match(l)
            if mm or (cur and re.match(r"^#{2,3} ", l)):
                if cur:
                    yield item(*cur[:6], "\n".join(cur[6]))
                cur = [mm.group(1), sev, mm.group(2), mm.group(3), closed, rel, []] if mm else None
            elif cur:
                cur[6].append(l)
        if cur:
            yield item(*cur[:6], "\n".join(cur[6]))


def export():
    items = list(units()) + list(issues())
    for it in items:
        it["hash"] = hashlib.sha1(json.dumps(it, ensure_ascii=False, sort_keys=True).encode()).hexdigest()[:12]
    return items


def load_state():
    state = {}
    try:
        for line in open(STATE, encoding="utf-8"):
            if line.strip():
                rec = json.loads(line)
                state[rec.pop("external_id")] = rec
    except FileNotFoundError:
        pass
    return state


def plan():
    state, ops, items = load_state(), [], export()
    for it in items:
        s = state.get(it["external_id"])
        if s and s.get("hash") == it["hash"]:
            continue
        if not s and it["state"] in ("Done", "Cancelled") and "unit" in it["labels"]:
            continue  # finished units were never mirrored; don't backfill them
        s = s or {}
        old = s.get("module")
        ops.append(dict(external_id=it["external_id"], plane_id=s.get("plane_id"), state=it["state"],
                        module=it["module"], old_module=old if old and old != it["module"] else None))
    known = {it["external_id"] for it in items}
    gone = sorted(k for k in state if k not in known)
    return ops, gone


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "export":
        json.dump(export(), sys.stdout, ensure_ascii=False, indent=1)
    elif cmd == "plan":
        ops, gone = plan()
        json.dump(dict(ops=ops, gone_from_repo=gone), sys.stdout, ensure_ascii=False, indent=1)
    elif cmd == "show":
        it = next((x for x in export() if x["external_id"] == argv[2]), None)
        if not it:
            sys.exit(f"unknown external_id: {argv[2]}")
        json.dump(it, sys.stdout, ensure_ascii=False, indent=1)
    elif cmd == "ack":
        it = next(x for x in export() if x["external_id"] == argv[2])
        os.makedirs(os.path.dirname(STATE), exist_ok=True)
        rec = dict(external_id=argv[2], plane_id=argv[3], hash=it["hash"], module=it["module"])
        with open(STATE, "a", encoding="utf-8") as f:  # one short O_APPEND write per ack
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
    elif cmd == "selftest":
        assert md2html("a **b\nc** d") == "<p>a <strong>b<br>c</strong> d</p>"
        assert md2html("| a | b |\n|---|---|\n| `x` | y |") == "<table><tr><th>a</th><th>b</th></tr><tr><td><code>x</code></td><td>y</td></tr></table>"
        items = export()
        ids = [x["external_id"] for x in items]
        assert len(ids) == len(set(ids)), "duplicate external_id"
        assert all("**" not in x["description_html"] for x in items), "unconverted bold"
        print(f"ok: {len(items)} items")
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
