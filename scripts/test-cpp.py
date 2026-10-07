#!/usr/bin/env python3
"""The C preprocessor running on Ouroboros: the C-hosting plan's finish line.

    python3 scripts/test-cpp.py      (or `make test-cpp`, which builds the image)

Step 8 of docs/roadmap/roadmap-c-hosting.md: `/bin/CPP` is DevTools's cpp,
built by `make cpp-bin` with /include:/include/clang built in. One boot of a
copy of build/esp.img, onto which the host first writes the inputs (/src):

- `cpp -o` of each input, with no options, must exit 0: hello.c includes
  <stdio.h>; picodemo.c is CPICO's source, a dozen headers deep; high.c has
  UTF-8 and bytes past 0x7f in a comment, a string and a character, where a
  lexer's `char` (unsigned here, signed on the Mac) could tell;
- after the boot the host reads the two .i files off the image, and each must
  be byte for byte what the SAME cpp, built for the Mac from CPP_DIR, writes
  there from the same staged headers (build/esp/include, its paths mapped to
  the guest's) and built with -funsigned-char as the target's char is, with
  SOURCE_DATE_EPOCH set on both sides: the port changes nothing;
- clang, for this target (CFLAGS_OS, PICO_INC), must compile what the guest
  wrote: a foreign observer that the output is the program;
- and `cpp /src/hello.c`, the handoff's Done when, prints it and exits 0.

Token for token against `clang -E` is DevTools's own `make pico-check`, on the
host, and not repeated here. No fault line in QEMU's trace. About a minute and
a half. Run it whenever the C runtime, the header stage or cpp-bin changes.
"""
import importlib.util
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

STAGED = os.path.join(ROOT, "build", "esp")
IMAGE = os.path.join(ROOT, "build", "esp.img")
COPY = os.path.join(ROOT, "build", "test-cpp.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-cpp.txt")
HOST = os.path.join(ROOT, "build", "test-cpp")
EPOCH = "1700000000"
HELLO = '#include <stdio.h>\n\nint main(void) {\n    printf("hello, world\\n");\n    return 0;\n}\n'
# Bytes past ASCII, which a lexer reads through `char`: signed on the Mac,
# unsigned for this target, so a lexer that leans on the sign would differ
# here and only with input like this (the review of #226).
HIGH = ('/* caf\u00e9, na\u00efve, \u00fcber: UTF-8 in a comment */\n'
        '#define GREETING "h\u00e9llo \\xff\\x80 \u00e5"\n'
        'const char *g = GREETING;\nconst char c = \'\\xe9\';\n')
SOURCES = {"hello.c": HELLO, "picodemo.c": open(os.path.join(ROOT, "libc", "picodemo.c")).read(),
           "high.c": HIGH}


def env(name):
    value = os.environ.get(name)
    if not value:
        sys.exit(f"test-cpp: {name} is not set (run it as `make test-cpp`)")
    return value


def attach(image, readonly):
    """Mount a FAT32 image on the host; returns the mount point."""
    mp = tempfile.mkdtemp(prefix="test-cpp.")
    args = ["hdiutil", "attach", "-nobrowse", "-mountpoint", mp, image]
    if readonly:
        args.insert(2, "-readonly")
    subprocess.run(args, check=True, capture_output=True)
    return mp


def detach(mp):
    """Detach, forcing it if macOS holds the volume (Spotlight, fseventsd), as
    the Makefile's image-stall does; the mount point is removed either way."""
    r = subprocess.run(["hdiutil", "detach", mp], capture_output=True)
    if r.returncode != 0:
        subprocess.run(["hdiutil", "detach", "-force", mp], check=True, capture_output=True)
    os.rmdir(mp)


def strip_sidecars(mp):
    """macOS's AppleDouble `._*` files, which `make image` strips too: fsd
    would list them, and they are not the inputs."""
    for dirpath, _, files in os.walk(mp):
        for f in files:
            if f.startswith("._"):
                os.remove(os.path.join(dirpath, f))


def host_cpp(cpp_dir):
    """The same cpp, for the Mac, from the same sources: no system directories
    built in, so the host run passes them, and target.h, as the guest's
    build does (the first system directory's target.h, read first). Built
    with -funsigned-char, as `char` is for this target and not on the Mac, so
    the two can differ only where the port does. Rebuilt only when a source
    (or this script) is newer than it."""
    os.makedirs(HOST, exist_ok=True)
    out = os.path.join(HOST, "cpp")
    lib = os.path.join(cpp_dir, "lib")
    srcs = sorted(os.path.join(lib, f) for f in os.listdir(lib) if f.endswith(".c")) + \
        [os.path.join(cpp_dir, "driver", "cpp.c")]
    inputs = srcs + [os.path.join(lib, f) for f in os.listdir(lib) if f.endswith(".h")] + [__file__]
    if os.path.exists(out) and os.path.getmtime(out) > max(os.path.getmtime(f) for f in inputs):
        return out
    subprocess.run(["cc", "-std=c11", "-O2", "-funsigned-char", "-I", lib, *srcs, "-o", out],
                   check=True)
    return out


def main() -> int:
    try:
        return run()
    finally:
        # The image copy goes whatever happened (the review of #226).
        if os.path.exists(COPY):
            os.remove(COPY)


def run() -> int:
    cpp_dir = env("CPP_DIR")
    flags = shlex.split(env("CFLAGS_OS")) + shlex.split(env("PICO_INC"))
    checks = []

    # The inputs, written onto a copy of the image by the host.
    shutil.copyfile(IMAGE, COPY)
    mp = attach(COPY, readonly=False)
    try:
        os.makedirs(os.path.join(mp, "src"), exist_ok=True)
        for name, text in SOURCES.items():
            with open(os.path.join(mp, "src", name), "w", encoding="utf-8") as fh:
                fh.write(text)
        strip_sidecars(mp)
    finally:
        detach(mp)

    guest = drive_qemu.Guest(COPY, label="test-cpp: ")
    try:
        steps = [("login:", "root"), ("assword", "root"), ("# ", f"set SOURCE_DATE_EPOCH={EPOCH}")]
        # Each prompt is the previous command's end; waiting on the exit line
        # as well would let its settle swallow that prompt.
        for name in SOURCES:
            stem = name[:-2]
            steps += [("# ", f"cpp -o /{stem}.i /src/{name}")]
        # The handoff's own Done when: `cpp hello.c`, printing the program.
        # Its end is the exit line: a prompt pattern would match the line
        # markers it prints (`# 29 "/include/..."`).
        steps += [("# ", "cpp /src/hello.c"), (r"exited \(code \d+\)", "")]
        driven = guest.run(steps)
        # cpp's output reaches the transcript through the console server,
        # the exit line straight from the kernel, so the printed tail can come
        # after the exit line: poll for it rather than snapshot at once.
        deadline = time.monotonic() + 15
        while 'printf("hello, world' not in guest.transcript().split("cpp /src/hello.c", 1)[-1] \
                and time.monotonic() < deadline:
            time.sleep(0.2)
        out = guest.transcript()
    finally:
        guest.stop()
    # After stop: QEMU buffers its -d int trace, so a fault in the last
    # command is only in the file once QEMU is gone (drive-qemu.py's main).
    faults = guest.aborts()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)
    checks.append(("driven to the end", driven))
    cmds = [f"cpp -o /{n[:-2]}.i /src/{n}" for n in SOURCES] + ["cpp /src/hello.c"]
    for name, cmd, nxt in zip(SOURCES, cmds, cmds[1:]):
        # This command's own exit line: after its own echo and before the next
        # command's, so a run that faulted (no exit line) cannot borrow the
        # next one's (the reviews of this rig).
        block = out.split(cmd, 1)[1].split(nxt, 1)[0] if cmd in out else ""
        code = re.search(r"exited \(code (\d+)\)", block)
        checks.append((f"cpp {name} on Ouroboros exited 0", bool(code) and code.group(1) == "0"))

    plain = out.split("cpp /src/hello.c", 1)[1] if "cpp /src/hello.c" in out else ""
    pcode = re.search(r"exited \(code (\d+)\)", plain)
    checks.append(("cpp /src/hello.c prints the program (main and its printf) and exits 0",
                   "int main(void)" in plain and 'printf("hello, world\\n")' in plain
                   and bool(pcode) and pcode.group(1) == "0"))

    # The guest's output, read off the image by the host.
    got = {}
    mp = attach(COPY, readonly=True)
    try:
        for name in SOURCES:
            path = os.path.join(mp, name[:-2] + ".i")
            got[name] = open(path, "rb").read() if os.path.exists(path) else None
    finally:
        detach(mp)

    # The same cpp on the Mac, from the same staged headers and the same
    # sources, under a host directory that stands in for the guest's root.
    root = os.path.join(HOST, "root")
    shutil.rmtree(root, ignore_errors=True)
    os.makedirs(os.path.join(root, "src"))
    shutil.copytree(os.path.join(STAGED, "include"), os.path.join(root, "include"))
    for name, text in SOURCES.items():
        with open(os.path.join(root, "src", name), "w", encoding="utf-8") as fh:
            fh.write(text)
    cpp = host_cpp(cpp_dir)
    for name in SOURCES:
        stem = name[:-2]
        host_out = os.path.join(HOST, stem + ".i")
        r = subprocess.run([cpp, "-include", f"{root}/include/target.h", "-I", f"{root}/include",
                            "-I", f"{root}/include/clang", "-o", host_out, f"{root}/src/{name}"],
                           env={**os.environ, "SOURCE_DATE_EPOCH": EPOCH}, capture_output=True)
        if r.returncode != 0:
            # A failure of the REFERENCE, said as such, not as the port's.
            print(f"     host cpp failed on {name} (exit {r.returncode}):\n{r.stderr.decode()[-600:]}")
        checks.append((f"{name}: the host cpp, the reference, ran", r.returncode == 0))
        want = open(host_out, "rb").read().replace(root.encode(), b"") if r.returncode == 0 else None
        g = got[name]
        checks.append((f"{name}: the guest wrote /{stem}.i ({len(g) if g else 0} bytes)", bool(g)))
        checks.append((f"{name}: the guest's output is the host cpp's, byte for byte",
                       g is not None and want is not None and g == want))
        if g:
            guest_i = os.path.join(HOST, stem + ".guest.i")
            with open(guest_i, "wb") as fh:
                fh.write(g)
            # cpp-output: clang takes the text as already preprocessed, so an
            # #include the guest left unexpanded is an error here, not quietly
            # resolved from the host's include path (the review of #226).
            c = subprocess.run(["clang", *flags, "-fsyntax-only", "-x", "cpp-output", guest_i],
                               capture_output=True, text=True)
            checks.append((f"{name}: clang compiles the guest's output", c.returncode == 0))
            if c.returncode != 0:
                print(c.stderr[-600:])
    checks.append(("no fault lines", faults == 0))

    failed = 0
    for what, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {what}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; outputs in {os.path.relpath(HOST, ROOT)}/")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
