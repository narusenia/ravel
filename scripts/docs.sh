#!/usr/bin/env bash
# Documentation search and consistency checks for Ravel.
#
# The docs are split by role (see docs/README.md): rules state what must hold,
# docs/dev states how to do it, specifications state intended behaviour,
# ui-impl-status states what works today, implementation states what order to
# build in, and issues track what is broken. This script finds hits per role so
# the answer's authority is visible, and resolves the repository's identifiers
# (unit IDs like PTR-3, issue IDs like MED-CORE-09).
#
# Usage:
#   scripts/docs.sh <query>          keyword search grouped by role
#   scripts/docs.sh id <ID>          resolve a unit or issue ID
#   scripts/docs.sh panel <name>     everything about one panel
#   scripts/docs.sh check            links, index coverage, ID consistency
#   scripts/docs.sh map              print the role map

set -uo pipefail
cd "$(git rev-parse --show-toplevel)"

if command -v rg >/dev/null 2>&1; then
    search() { rg --no-heading --line-number --color=never "$@"; }
else
    search() { # $@ = pattern then paths
        local pattern="$1"
        shift
        grep -rniE --line-number "$pattern" "$@" 2>/dev/null
    }
fi

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
dim() { printf '\033[2m%s\033[0m\n' "$1"; }

# Role → paths. Order is authority order: rules first, plans last.
ROLE_NAMES=(
    "規範 (.agents/rules)"
    "手順 (docs/dev)"
    "参照 (API / GPUI)"
    "設計意図 (docs/specifications)"
    "実装状況 (docs/ui-impl-status.md)"
    "要件 (docs/requirements)"
    "計画 (docs/implementation)"
    "課題 (issues)"
)
ROLE_PATHS=(
    ".agents/rules"
    "docs/dev"
    "docs/agent-api-reference.md docs/gpui-ui-guide.md"
    "docs/specifications"
    "docs/ui-impl-status.md"
    "docs/requirements"
    "docs/implementation"
    "issues"
)

cmd_search() {
    local query="$1" found=0
    for i in "${!ROLE_NAMES[@]}"; do
        # shellcheck disable=SC2086 # paths are intentionally word-split
        local hits
        hits=$(search "$query" ${ROLE_PATHS[$i]} 2>/dev/null)
        [ -z "$hits" ] && continue
        found=1
        bold "── ${ROLE_NAMES[$i]}"
        printf '%s\n' "$hits" | head -20
        local total
        total=$(printf '%s\n' "$hits" | wc -l | tr -d ' ')
        [ "$total" -gt 20 ] && dim "   … 他 $((total - 20)) 行"
        echo
    done
    if [ "$found" -eq 0 ]; then
        echo "no hits for: $query"
        return 1
    fi
    dim "上にあるものほど強い（規範 > 手順 > 設計意図 > 計画）。実装と食い違う文書は実装が正。"
}

cmd_id() {
    local id="$1"
    bold "── $id"
    case "$id" in
    CRIT-* | HIGH-* | MED-* | LOW-*)
        search "$id" issues docs | head -30
        echo
        dim "個票が正。着手順は docs/implementation/roadmap.md、引受先は個票に記載。"
        ;;
    *)
        echo "backlog:"
        search "\| *$id *\|" docs/implementation/backlog.md | head -5
        echo
        echo "計画書:"
        search "$id" docs/implementation --glob '*-plan.md' 2>/dev/null | head -15 ||
            grep -rn "$id" docs/implementation/*-plan.md 2>/dev/null | head -15
        echo
        echo "roadmap:"
        search "$id" docs/implementation/roadmap.md | head -8
        echo
        dim "単位の内容と完了条件は計画書が正。状態は backlog、順序は roadmap。"
        ;;
    esac
}

cmd_panel() {
    local name="$1"
    local spec="docs/specifications/ui/$name.md"

    bold "── 設計意図"
    if [ -f "$spec" ]; then
        echo "  $spec"
    else
        echo "  ($spec が無い — docs/specifications/ui-spec.md の索引を確認)"
    fi

    bold "── 実装状況 (docs/ui-impl-status.md)"
    grep -n -i "$name" docs/ui-impl-status.md | cut -c1-160 | head -8

    bold "── 未実装項目と担当"
    if [ -f "$spec" ]; then
        # The spec's trailing "未実装項目" section, table rows only.
        awk '/^## .*未実装/ {inside=1; next} /^## / {inside=0} inside && /^\|/' "$spec" |
            grep -v '^| *---' | grep -v '^| *項目' | sed 's/^/  /' | head -15
    fi

    bold "── 関連計画（言及の多い順）"
    grep -rc -i "$name" docs/implementation/*-plan.md 2>/dev/null |
        awk -F: '$2 > 0 {printf "%5d  %s\n", $2, $1}' | sort -rn | head -6

    bold "── 関連 issue"
    grep -rn "panels/$name" issues 2>/dev/null |
        sed -E 's/^(issues\/[^:]+):([0-9]+):.*/  \1:\2/' | sort -u | head -8
}

cmd_check() {
    local failures=0

    bold "── リンク切れ"
    if python3 - <<'PY'; then
import glob, os, re, sys
bad = []
targets = glob.glob('docs/**/*.md', recursive=True) + glob.glob('issues/**/*.md', recursive=True)
targets += ['AGENTS.md', 'CLAUDE.md', 'README.md'] + glob.glob('.agents/rules/*.md')
for path in targets:
    if not os.path.exists(path):
        continue
    base = os.path.dirname(path)
    for m in re.finditer(r'\[[^\]]*\]\(([^)#]+\.md)\)', open(path, encoding='utf-8').read()):
        target = m.group(1)
        if target.startswith('http'):
            continue
        if not os.path.exists(os.path.normpath(os.path.join(base, target))):
            bad.append(f'{path} -> {target}')
for entry in bad:
    print(f'  MISSING {entry}')
print(f'  {len(targets)} files checked, {len(bad)} broken')
sys.exit(1 if bad else 0)
PY
        :
    else
        failures=$((failures + 1))
    fi

    bold "── 索引から辿れない文書"
    local orphans=0
    while IFS= read -r doc; do
        local name
        name=$(basename "$doc")
        # Indexes that are allowed to introduce a document.
        if ! grep -rq "$name" docs/README.md docs/dev/README.md docs/implementation/README.md \
            docs/specifications/ui-spec.md AGENTS.md 2>/dev/null; then
            echo "  ORPHAN $doc"
            orphans=$((orphans + 1))
        fi
    done < <(find docs -name '*.md' \
        -not -path 'docs/implementation/archive/*' \
        -not -path 'docs/implementation/done/*' \
        -not -name 'README.md')
    echo "  $orphans orphaned"
    [ "$orphans" -gt 0 ] && failures=$((failures + 1))

    # Informational only: backlog and roadmap usually cite plans by unit ID
    # (ALIGN-1, BLUR-3, …) rather than by filename, so a hit here is a hint to
    # check by hand, not a failure.
    bold "── backlog / roadmap にファイル名で出てこない計画書（要確認）"
    local unlisted=0
    for plan in docs/implementation/*-plan.md; do
        local name
        name=$(basename "$plan")
        if ! grep -q "$name" docs/implementation/backlog.md docs/implementation/roadmap.md 2>/dev/null; then
            echo "  CHECK $name"
            unlisted=$((unlisted + 1))
        fi
    done
    echo "  $unlisted 件（単位 ID で参照されているなら問題ない）"

    bold "── issue 件数と索引の一致"
    for sev in critical high medium low; do
        local counted
        case "$sev" in
        critical | high) counted=$(find "issues/$sev" -name '*.md' 2>/dev/null | wc -l | tr -d ' ') ;;
        *) counted=$(grep -rhoE "^#{2,3} (MED|LOW)-[A-Z]+-[0-9]+|^\*\*(MED|LOW)-[A-Z]+-[0-9]+" "issues/$sev" 2>/dev/null | wc -l | tr -d ' ') ;;
        esac
        local declared
        declared=$(grep -E "^\| $sev " issues/README.md | grep -oE '[0-9]+' | head -1)
        printf '  %-9s 実体 %-4s 索引 %s\n' "$sev" "$counted" "${declared:-?}"
    done
    dim "  （low / medium は 1 ファイル複数項目なので見出し数で数える。解決済みを含む）"

    echo
    if [ "$failures" -eq 0 ]; then
        echo "docs check: clean"
    else
        echo "docs check: $failures 種類の問題"
    fi
    return "$failures"
}

cmd_map() {
    bold "文書の役割（docs/README.md より）"
    cat <<'EOF'
  規範     .agents/rules/        守るべきこと。lint と ravel-review が強制
  手順     docs/dev/             何を触るか。チェックリストが本体
  参照     docs/agent-api-reference.md, docs/gpui-ui-guide.md
  設計意図 docs/specifications/  どう振る舞うべきか（ui/ はビュー別）
  実装状況 docs/ui-impl-status.md 今どこまで動くか
  要件     docs/requirements/    REQ-<領域>-<番号>
  計画     docs/implementation/  backlog=何があるか / roadmap=どの順で / *-plan.md=設計
  課題     issues/               何が壊れているか（着手順は持たない）

  食い違ったときは実装が正。文書の更新義務は docs/dev/doc-checklist.md。
EOF
}

case "${1:-}" in
"" | -h | --help)
    sed -n '3,17p' "$0" | sed 's/^# \{0,1\}//'
    ;;
id)
    [ $# -ge 2 ] || {
        echo "usage: scripts/docs.sh id <PTR-3|MED-CORE-09>" >&2
        exit 2
    }
    cmd_id "$2"
    ;;
panel)
    [ $# -ge 2 ] || {
        echo "usage: scripts/docs.sh panel <viewer|timeline|node-editor|outliner|properties|media-bin>" >&2
        exit 2
    }
    cmd_panel "$2"
    ;;
check) cmd_check ;;
map) cmd_map ;;
*) cmd_search "$1" ;;
esac
