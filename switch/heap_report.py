#!/usr/bin/env python3
"""Sum the OOM heap samples in boot.txt by owning function.

usage: switch/heap_report.py [boot.txt] [depth]
Owner = first `depth` (default 1) stack frames past allocator plumbing.
"""
import collections
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
ELF = ROOT.parent / "target/nx/iw4l.sym.elf"
NOISE = re.compile(
    r"^(\?\?|__wrap_|__real_|_malloc|_memalign|_free|_realloc|_calloc|malloc|calloc|realloc|memalign|"
    r"rmutex|__rustc|platform_nx::|<?diag::|<?alloc::|<?core::|<?std::|<\[|<?hashbrown::|<?allocator_api2)"
)

boot = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "boot.txt"
depth = int(sys.argv[2]) if len(sys.argv) > 2 else 1
samples = [[w for w in line.split()[1:] if w != "0"] for line in boot.read_text().splitlines() if line.startswith("smp ")]
if not samples:
    sys.exit(f"no `smp` lines in {boot}")

# Stack words are return addresses: look up the call instruction before them.
pcs = sorted({pc for s in samples for pc in s[2:]})
out = subprocess.run(
    ["addr2line", "-f", "-C", "-e", str(ELF), *(hex(int(pc, 16) - 4) for pc in pcs)],
    capture_output=True, text=True, check=True,
).stdout.splitlines()
name = {pc: re.sub(r"::\{closure.*", "", fn)[:120] for pc, fn in zip(pcs, out[0::2])}

totals = collections.Counter()
counts = collections.Counter()
for weight, _size, *stack in samples:
    frames = [name[pc] for pc in stack if not NOISE.match(name[pc])]
    owner = " <- ".join(dict.fromkeys(frames[:depth])) or "(unknown)"
    totals[owner] += int(weight)
    counts[owner] += 1
total = sum(totals.values())
print(f"{total >> 20}MB sampled live in {len(samples)} samples")
for owner, weight in totals.most_common(50):
    print(f"{weight / (1 << 20):8.1f}MB {counts[owner]:5d}  {owner}")
