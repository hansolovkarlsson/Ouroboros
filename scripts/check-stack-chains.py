#!/usr/bin/env python3
"""Every userland program's deepest call chain against the stack it is given.

The loader gives each task one fixed stack (`STACK_PAGES` in
kernel/src/loader.rs) with a guard page below it, and a program that goes
deeper dies of a fault at whatever moment it does. Four times the stack was
grown after such a fault. On 2026-11-11 `netd` was found 1,168 bytes from the
guard, by one `resolve <name>` that no rig ran (docs/ROADMAP.md): the rigs
read the mark of the paths they drive, and the deepest path was not one.

This reads the depth off the built binaries instead, before any boot. For
each ELF it disassembles the program, takes every function's frame (the
stack pointer's decrements in its body, a probed frame's page loop included),
and follows every direct call from `_start` to the deepest chain. A program
whose deepest chain leaves less than its floor fails.

What it cannot see, and says so in its output:

- a function no symbol names is found only where a `bl` names its address;
  one reached by nothing but a branch or a pointer is read as part of the
  function before it, its frame added there;
- an indirect call (`blr`) is counted, not followed. Today those are
  `core::fmt`'s padding and the C library's `printf`;
- a recursive program has no deepest chain. Its chain is taken with each
  cycle walked once, and the program must be named in RECURSIVE below, so a
  program that becomes recursive fails until someone has looked;
- a frame sized at run time (`sub sp, sp, x1`) fails outright: none exists
  today, and one would make every figure here a guess.

The figure has been exact where it was checked. On 2026-11-11 `netd`'s chain
through `handle_client` was 52,064 bytes by this reading and
`/proc/4/stackfree` read 5,280 of 57,344 left at the prompt; its deepest was
56,176 and the mark read 1,168 after `resolve <name>`. And `stackprobe`'s
`dig` frame is the 1,072 bytes `make test-stack-free` measures a level. The second is asserted
here on every run (the canary): a reading that stopped seeing frames would
otherwise report every program shallow.

Usage:  python3 scripts/check-stack-chains.py [--self-test] [--chain NAME] [ELF...]
        (make check-stack-chains; the binaries are `make esp`'s)
Exit status: 0 when every program keeps its floor, 1 otherwise, 2 when the
check could not run.
"""
import glob
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

# The least a program's deepest chain may leave of its stack, in bytes.
FLOOR = 4096
# Programs below FLOOR today, each with the least it may leave and why it is
# let stand. A program here that comes to keep FLOOR fails too, so the entry
# is removed and cannot rot.
BELOW = {
    # None today. netd stood here at 1,024 until its tables left serve's
    # frame for the heap (2026-11-11): 1,168 left before, 37,296 after.
}
# Programs with a call cycle. Each was looked at: the number is the most
# times its cycles can be walked, read from the source, and the chain is
# then taken with every cycle's frames counted that many times and held to
# the floor. Such an entry names its cycles, each as the functions in it,
# and the program fails when its cycles are any others: a second recursion
# in fsd is not covered by the bound written for the first (the review of
# #441). None is a recursion bounded only by the program's input, which
# this check cannot hold to anything: it is printed on every run, so the
# list of them stays in view, and the floor is not asked of it.
RECURSIVE = {
    # ext2's free_pointed: one call a level of pointer block, and its two
    # callers start it at depth 0 (single indirect) and 1 (double): this
    # ext2 has no triple-indirect files.
    "fsd": (2, "ext2::free_pointed, one call a level of indirect block, two at most", [["free_pointed"]]),
    "tree": (None, "walk, one call (1,312 bytes) a directory level: about forty levels deep is the guard page"),
    "grep": (None, "the regex compiler (parse_alt, parse_repeat, nullable, Emitter::gen), one round a nesting of the pattern"),
    "stackprobe": (None, "dig, as deep as its argument says: it is the stack rig's probe"),
    "cpico": (None, "picolibc's qsort"),
    # CPP's and DevTools's programs, built here in four toolchain variants
    # each; their recursion is those projects' to bound.
    "cpp": (None, "CPP's expression parser and macro expander, by the input's nesting"),
    "cpp-ld": (None, "as cpp"),
    "cpp-cc": (None, "as cpp"),
    "cpp-ld-cc": (None, "as cpp"),
    "edit": (None, "DevTools's editor: command and repeat, by the command typed"),
    "edit-cc": (None, "as edit"),
    "edit-ld": (None, "as edit"),
    "edit-ld-cc": (None, "as edit"),
}

# stackprobe's `dig`: 1 KiB of buffer and its few words, the frame the stack
# rig measures. The canary's bounds are that rig's (CHUNK, FRAME_EXTRA).
CANARY_PROGRAM, CANARY_FUNCTION, CANARY_BOUNDS = "stackprobe", "dig", (1024, 1152)


def stack_bytes():
    """The stack every task gets, from the loader's own constant."""
    src = open(os.path.join(ROOT, "kernel", "src", "loader.rs")).read()
    m = re.search(r"^const STACK_PAGES: u64 = (\d+);", src, re.M)
    if not m:
        sys.exit("check-stack-chains: STACK_PAGES not found in kernel/src/loader.rs")
    return int(m.group(1)) * 4096


def objdump():
    found = glob.glob(os.path.expanduser("~/.rustup/toolchains/*/lib/rustlib/*/bin/llvm-objdump"))
    if not found:
        print("check-stack-chains: no llvm-objdump (rustup component add llvm-tools)", file=sys.stderr)
        sys.exit(2)
    return sorted(found)[0]


def parse(text):
    """Disassembly -> {function: {"frame", "calls", "blr", "dyn"}}.

    A function is keyed by its name, or by `name@address` for the second and
    later of a name: C's static functions share names across files (two
    `open`s made one function with both sets of calls, and a cycle that was
    not there). Calls are followed by address.

    A function starts at a symbol, and also at any address a `bl` names that
    no symbol does (`anon@address`). DevTools's linker keeps no symbols for
    static functions, so in a binary it linked a call to one reads as
    `bl <stat+0x410>`: with symbols alone half of such a program's calls led
    nowhere, its static functions' frames were added to whichever symbol
    came before them, and its recursion was not seen (the review of #441).
    """
    # Pass 1: the instructions, and where the functions start.
    starts = {}  # address -> name
    insns = []  # (address, text)
    seen = set()
    for line in text.splitlines():
        m = re.match(r"^([0-9a-f]+) <(.+)>:$", line)
        if m:
            addr, name = int(m.group(1), 16), m.group(2)
            if name in seen:
                name = f"{name}@{addr:x}"
            seen.add(name)
            starts[addr] = name
            continue
        m = re.match(r"^\s*([0-9a-f]+):\s+(.*)$", line)
        if m:
            insns.append((int(m.group(1), 16), m.group(2).strip()))
    for addr, ins in insns:
        m = re.match(r"bl\t0x([0-9a-f]+)\b", ins)
        if m:
            target = int(m.group(1), 16)
            # A `bl` to the next instruction is a call to a weak symbol
            # nothing defined, left as a no-op: not a function.
            if target not in starts and target != addr + 4:
                starts[target] = f"anon@{target:x}"

    # Pass 2: each instruction to the function it lies in.
    funcs = {name: {"frame": 0, "calls": set(), "blr": 0, "dyn": [], "probe": None} for name in starts.values()}
    cur = None
    for addr, ins in insns:
        if addr in starts:
            cur = starts[addr]
        if cur is None:
            continue
        f = funcs[cur]
        # A probed frame: `sub x9, sp, #N, lsl #12`, then a loop that takes
        # sp down a page at a time until it is x9. The loop's one `sub sp` is
        # counted by the rule below, so N less that page is counted here.
        m = re.match(r"sub\tx(\d+), sp, #(0x[0-9a-f]+|\d+), lsl #12", ins)
        if m:
            f["probe"] = (m.group(1), (int(m.group(2), 0) << 12) - 4096)
            continue
        if f["probe"] and re.match(rf"cmp\tsp, x{f['probe'][0]}\b", ins):
            f["frame"] += f["probe"][1]
            f["probe"] = None
            continue
        m = re.match(r"sub\tsp, sp, #(0x[0-9a-f]+|\d+)(, lsl #12)?", ins)
        if m:
            f["frame"] += int(m.group(1), 0) << (12 if m.group(2) else 0)
            continue
        # Writeback that lowers sp: pre-indexed (`[sp, #-N]!`), and
        # post-indexed (`[sp], #-N`), which a probe of one more page uses.
        m = re.search(r"\[sp, #-(0x[0-9a-f]+|\d+)\]!", ins) or re.search(r"\[sp\], #-(0x[0-9a-f]+|\d+)", ins)
        if m:
            f["frame"] += int(m.group(1), 0)
            continue
        # Any other write of sp that is not giving a frame back.
        if re.match(r"(sub|and|mov)\tsp, ", ins) and not re.match(r"mov\tsp, x29\b", ins):
            f["dyn"].append(ins.replace("\t", " "))
            continue
        m = re.match(r"(bl|b)\t0x([0-9a-f]+)\b", ins)
        if m:
            # A `bl` anywhere, or a `b` to another function's first
            # instruction (a tail call, counted as a call: the caller's frame
            # may or may not be given back first, and deeper is the safe
            # side). A `b` to the function's own first instruction is a loop
            # (the panic handler's `loop {}`, a C `for (;;)`), not a call.
            target = int(m.group(2), 16)
            if target in starts and (m.group(1) == "bl" or starts[target] != cur):
                f["calls"].add(starts[target])
            continue
        if re.match(r"blr\b", ins):
            f["blr"] += 1
    return funcs


def deepest(funcs, entry="_start"):
    """(bytes, chain, cycles): the deepest direct chain from `entry`, and
    the call cycles reachable from it (each a sorted list of functions).

    The functions are grouped into strongly connected components (Tarjan),
    which leaves a graph with no cycle, and the deepest path through that is
    taken with a component weighing all its members' frames once. For a
    program with no cycle every component is one function and the figure is
    exact; for a recursive one it is a floor on the depth, not a bound.
    """
    index, low, comp, stack, on = {}, {}, {}, [], set()
    comps = []
    # Iterative Tarjan: C programs' call graphs are deep enough to matter.
    for root in [entry]:
        if root not in funcs or root in index:
            continue
        work = [(root, iter(sorted(funcs[root]["calls"])))]
        index[root] = low[root] = len(index)
        stack.append(root)
        on.add(root)
        while work:
            fn, it = work[-1]
            for t in it:
                if t not in funcs:
                    continue
                if t not in index:
                    index[t] = low[t] = len(index)
                    stack.append(t)
                    on.add(t)
                    work.append((t, iter(sorted(funcs[t]["calls"]))))
                    break
                if t in on:
                    low[fn] = min(low[fn], index[t])
            else:
                work.pop()
                if work:
                    parent = work[-1][0]
                    low[parent] = min(low[parent], low[fn])
                if low[fn] == index[fn]:
                    members = []
                    while True:
                        m = stack.pop()
                        on.discard(m)
                        comp[m] = len(comps)
                        members.append(m)
                        if m == fn:
                            break
                    comps.append(sorted(members))
    # Tarjan emits a component after every component it calls, so one pass
    # in emission order has each callee's depth ready.
    depth = []
    for i, members in enumerate(comps):
        own = sum(funcs[m]["frame"] for m in members)
        best = (0, [])
        for m in members:
            for t in funcs[m]["calls"]:
                if t in comp and comp[t] != i and depth[comp[t]][0] > best[0]:
                    best = depth[comp[t]]
        depth.append((own + best[0], members + best[1]))
    cycles = [m for m in comps if len(m) > 1 or m[0] in funcs[m[0]]["calls"]]
    if entry not in comp:
        return 0, [], cycles
    total, chain = depth[comp[entry]]
    return total, chain, cycles


def short(name):
    """A mangled Rust name cut to something a line can hold."""
    if not name.startswith("_R"):
        return name
    out = []
    i = 0
    # v0 mangling spells an identifier as its length then its bytes.
    while i < len(name):
        m = re.match(r"(\d+)_?", name[i:])
        if m and name[i - 1:i] != "j":
            k = int(m.group(1))
            ident = name[i + len(m.group(0)):i + len(m.group(0)) + k]
            if k and len(ident) == k and re.match(r"[A-Za-z_][A-Za-z0-9_]*$", ident):
                out.append(ident)
                i += len(m.group(0)) + k
                continue
        i += 1
    return "::".join(out[:4]) if out else name[:60]


def plain(name):
    """A function's own name: the last identifier of a mangled Rust path."""
    if not name.startswith("_R"):
        return name
    # v0 mangling spells an identifier as its length then its bytes, and a
    # function's own name is the one that ends the symbol.
    for m in re.finditer(r"\d+", name):
        ident = name[m.end():]
        if len(ident) == int(m.group(0)) and re.match(r"[A-Za-z_][A-Za-z0-9_]*$", ident):
            return ident
    return name


def find(funcs, plain):
    """The functions whose Rust path ends in `plain`."""
    return [n for n in funcs if re.search(rf"\d+{re.escape(plain)}(?![a-z0-9_])", n) or n == plain]


def program_of(path):
    return re.sub(r"\.elf$", "", os.path.basename(path))


def binaries():
    out = []
    for path in sorted(glob.glob(os.path.join(ROOT, "target", "aarch64-unknown-none", "release", "*"))) + \
            sorted(glob.glob(os.path.join(ROOT, "build", "*.elf"))):
        if not os.path.isfile(path) or path.endswith((".bin", ".d")):
            continue
        with open(path, "rb") as f:
            if f.read(4) != b"\x7fELF":
                continue
        out.append(path)
    return out


SELF_TEST = """
0000000000000000 <_start>:
       0:      \tstr\tx30, [sp, #-0x10]!
       4:      \tbl\t0x100 <big>
       8:      \tbl\t0x200 <small>

0000000000000100 <big>:
     100:      \tstp\tx29, x30, [sp, #-0x60]!
     104:      \tsub\tx9, sp, #0xa, lsl #12   // =0xa000
     108:      \tsub\tsp, sp, #0x1, lsl #12   // =0x1000
     10c:      \tcmp\tsp, x9
     110:      \tstr\txzr, [sp]
     114:      \tb.ne\t0x108 <big+0x8>
     118:      \tsub\tsp, sp, #0x8d0
     11c:      \tbl\t0x300 <leaf>
     120:      \tadd\tsp, sp, #0x8d0

0000000000000200 <small>:
     200:      \tsub\tsp, sp, #0x1, lsl #12   // =0x1000
     204:      \tsub\tsp, sp, #0x800
     208:      \tb\t0x300 <leaf>
     20c:      \tb\t0x204 <small+0x4>

0000000000000300 <leaf>:
     300:      \tsub\tsp, sp, #0x40
     304:      \tblr\tx8
     308:      \tbl\t0x30c <leaf+0xc>
     30c:      \tbl\t0x400 <leaf+0x100>
     310:      \tret
     400:      \tstr\txzr, [sp], #-0x100
     404:      \tret
"""


def self_test():
    """The reading against a disassembly whose answer is known by hand."""
    funcs = parse(SELF_TEST)
    want = {"_start": 16, "big": 0x60 + 0xa000 + 0x8d0, "small": 0x1800, "leaf": 0x40, "anon@400": 0x100}
    got = {n: funcs[n]["frame"] for n in want}
    total, chain, cycles = deepest(funcs)
    ok = got == want and total == 16 + want["big"] + 0x40 + 0x100 and chain == ["_start", "big", "leaf", "anon@400"] \
        and funcs["leaf"]["blr"] == 1 and cycles == [] and not any(f["dyn"] for f in funcs.values())
    looped = parse(SELF_TEST.replace("\t0x204 <small+0x4>\n", "\t0x100 <big>\n")
                   .replace("bl\t0x300 <leaf>\n     120", "bl\t0x200 <small>\n     120"))
    ok = ok and deepest(looped)[2] == [["big", "small"]]
    selfcall = parse(SELF_TEST.replace("blr\tx8", "bl\t0x300 <leaf>"))
    ok = ok and deepest(selfcall)[2] == [["leaf"]]
    # A cycle through a function no symbol names.
    hidden = parse(SELF_TEST.replace("404:      \tret", "404:      \tbl\t0x400 <leaf+0x100>"))
    ok = ok and deepest(hidden)[2] == [["anon@400"]]
    dyn = parse(SELF_TEST.replace("sub\tsp, sp, #0x40", "sub\tsp, sp, x1"))
    ok = ok and dyn["leaf"]["dyn"] != []
    print(f"check-stack-chains self-test: {'ok' if ok else 'FAILED'}"
          + ("" if ok else f" (frames {got}, want {want}; deepest {total} by {chain})"))
    return ok


def main():
    args = sys.argv[1:]
    if not self_test():
        return 1
    if args == ["--self-test"]:
        return 0
    show = None
    if "--chain" in args:
        i = args.index("--chain")
        show = args[i + 1]
        del args[i:i + 2]
    paths = args or binaries()
    if not paths:
        print("check-stack-chains: no binaries - build the userland first (make esp)", file=sys.stderr)
        return 2
    stack = stack_bytes()
    tool = objdump()
    failed = 0
    canary = False
    seen = set()
    rows = []
    for path in paths:
        name = program_of(path)
        seen.add(name)
        text = subprocess.run([tool, "-d", "--no-show-raw-insn", path], capture_output=True, text=True).stdout
        funcs = parse(text)
        if "_start" not in funcs:
            print(f"  FAIL {name}: no _start in its disassembly, so nothing was read")
            failed += 1
            continue
        total, chain, cycles = deepest(funcs)
        times, why, *named = RECURSIVE.get(name, (1, ""))
        bounded = not cycles or (name in RECURSIVE and times is not None)
        if cycles and bounded:
            total += (times - 1) * sum(funcs[m]["frame"] for cy in cycles for m in cy)
        left = stack - total
        indirect = sum(1 for f in funcs.values() if f["blr"])
        dyn = [(n, f["dyn"]) for n, f in funcs.items() if f["dyn"]]
        floor = BELOW.get(name, FLOOR)
        notes = []
        if dyn:
            failed += 1
            notes.append(f"FAIL a frame sized at run time in {short(dyn[0][0])}: {dyn[0][1][0]}")
        if cycles and name not in RECURSIVE:
            failed += 1
            notes.append(f"FAIL {len(cycles)} call cycle(s), the first through "
                         + ", ".join(short(c) for c in cycles[0])[:120]
                         + ": no deepest chain; look, bound it, and name the program in RECURSIVE")
        if cycles and named:
            got = sorted(sorted(plain(m) for m in cy) for cy in cycles)
            if got != sorted(sorted(cy) for cy in named[0]):
                failed += 1
                notes.append(f"FAIL its cycles are {got}, and its bound was written for {named[0]}: look again")
        if not cycles and name in RECURSIVE:
            failed += 1
            notes.append("FAIL named in RECURSIVE and has no call cycle: remove the entry")
        if cycles and name in RECURSIVE:
            notes.append((f"its cycles counted {times} times: " if bounded else "NOT BOUNDED, the floor not asked: ") + why)
        if not bounded:
            pass
        elif left < floor:
            failed += 1
            notes.append(f"FAIL {left} bytes left, under its floor of {floor}")
        elif name in BELOW and left >= FLOOR:
            failed += 1
            notes.append(f"FAIL named in BELOW and keeps the floor ({left} of {FLOOR}): remove the entry")
        if name == CANARY_PROGRAM:
            frames = [funcs[n]["frame"] for n in find(funcs, CANARY_FUNCTION)]
            canary = len(frames) == 1 and CANARY_BOUNDS[0] <= frames[0] <= CANARY_BOUNDS[1]
            if not canary:
                notes.append(f"FAIL the canary: `{CANARY_FUNCTION}`'s frame read as {frames}, want one of "
                             f"{CANARY_BOUNDS[0]} to {CANARY_BOUNDS[1]}")
        rows.append((left if bounded else stack, name, total, indirect, bool(cycles), notes, chain, funcs))

    for left, name, total, indirect, cyc, notes, chain, funcs in sorted(rows, key=lambda r: (r[0], r[1])):
        flag = "FAIL" if any(n.startswith("FAIL") for n in notes) else "ok  "
        extra = (f", {indirect} function(s) with indirect calls" if indirect else "") + (", recursive" if cyc else "") \
            + (f", floor {BELOW[name]}" if name in BELOW else "")
        loud = flag == "FAIL" or left < 2 * FLOOR or name == show or cyc
        if loud:
            print(f"  {flag} {name}: deepest chain {total}" + (f", {left} left" if left != stack or not cyc else "") + extra)
        for n in notes:
            if loud:
                print(f"         {n}")
        if flag == "FAIL" or name == show:
            run = 0
            for fn in chain:
                run += funcs[fn]["frame"]
                print(f"         {funcs[fn]['frame']:7d} {run:7d}  {short(fn)}")
    for name in sorted(set(BELOW) | set(RECURSIVE)):
        if name not in seen and not args:
            failed += 1
            print(f"  FAIL {name}: named in this script's tables and not among the binaries")
    if not args and not canary:
        failed += 1
        print(f"  FAIL the canary: {CANARY_PROGRAM}'s `{CANARY_FUNCTION}` frame was not read as the stack rig "
              "measures it, so no figure here is trusted")
    lefts = sorted((r[0], r[1]) for r in rows if r[0] != stack or not r[4])
    unbounded = sum(1 for r in rows if r[4] and r[0] == stack)
    summary = f"{len(rows)} programs, stack {stack}, floor {FLOOR}, {unbounded} recursive and not bounded; " \
        "shallowest margins: " + ", ".join(f"{n} {l}" for l, n in lefts[:4])
    if failed:
        print(f"check-stack-chains: {failed} failure(s); {summary}")
        return 1
    print(f"check-stack-chains: {summary} (canary confirmed)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
