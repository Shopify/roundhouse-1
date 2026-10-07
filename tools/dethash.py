"""Point every HashMap/HashSet in a Rust tree at the deterministic aliases
in `src/det.rs`, so iteration order is the same on every run.

Usage: dethash.py ROOT_PREFIX FILE...   (ROOT_PREFIX is `crate` or `roundhouse`)

From the repository root, after merging code that adds std hash maps:

    uv run --no-project python tools/dethash.py crate $(fd -e rs . src --exclude bin --exclude det.rs)
    uv run --no-project python tools/dethash.py roundhouse $(fd -e rs . src/bin tests)

Idempotent. Rewrites `std::collections::{HashMap, HashSet}` imports and
paths, and `X::new` / `X::with_capacity(n)` / `X::from([..])` for those
types and their local `type` aliases into the `default` /
`with_capacity_and_hasher` / `from_iter` forms a non-std hasher needs.
`[..].into()` conversions and nested `use std::{collections::..}` trees
still need a hand edit; `cargo check --all-targets` finds them.
"""
import re, sys
prefix = sys.argv[1]
GROUP = re.compile(r'(?P<ind>[ \t]*)(?P<pub>pub(?:\([^)]*\))? )?use std::collections::\{(?P<items>[^}]*)\};')
def fix_group(m):
    items = [i.strip() for i in m.group('items').split(',') if i.strip()]
    ours = [i for i in items if i in ('HashMap', 'HashSet')]
    if not ours:
        return m.group(0)
    rest = [i for i in items if i not in ours]
    ind, pub = m.group('ind'), m.group('pub') or ''
    out = []
    if rest:
        out.append(f"{ind}{pub}use std::collections::{{{', '.join(rest)}}};" if len(rest) > 1 else f"{ind}{pub}use std::collections::{rest[0]};")
    out.append(f"{ind}{pub}use {prefix}::det::{{{', '.join(ours)}}};" if len(ours) > 1 else f"{ind}{pub}use {prefix}::det::{ours[0]};")
    return '\n'.join(out)
def with_capacity(s, tf):
    pat = re.compile(r'\b(' + NAMES + tf + r')::with_capacity\(')
    out, pos = [], 0
    while (m := pat.search(s, pos)):
        depth, i = 1, m.end()
        while depth:
            depth += {'(': 1, ')': -1}.get(s[i], 0)
            i += 1
        arg = s[m.end():i - 1]
        out.append(s[pos:m.start()] + f"{m.group(1)}::with_capacity_and_hasher({arg}, Default::default())")
        pos = i
    out.append(s[pos:])
    return ''.join(out)
ALIAS = re.compile(r'\btype\s+(\w+)\s*(?:<[^=]*>)?\s*=\s*(?:[\w:]*::)?Hash(?:Map|Set)\s*<')
names = {'HashMap', 'HashSet'}
paths = [p for p in sys.argv[2:] if not p.endswith('src/det.rs')]
for path in paths:
    names |= set(ALIAS.findall(open(path).read()))
NAMES = '(?:' + '|'.join(sorted(names)) + ')'
for path in paths:
    s = open(path).read()
    o = s
    s = GROUP.sub(fix_group, s)
    s = re.sub(r'\bstd::collections::(HashMap|HashSet)\b', rf'{prefix}::det::\1', s)
    if True:
        tf = r'(?:::<(?:[^<>()]|<(?:[^<>()]|<[^<>()]*>)*>)*>)?'
        s = re.sub(r'\b(' + NAMES + tf + r')::new\b', r'\1::default', s)
        s = with_capacity(s, tf)
        s = re.sub(r'\b(' + NAMES + tf + r')::from\(', r'\1::from_iter(', s)
    if s != o:
        open(path, 'w').write(s)
