#!/bin/sh
# Scripted real-hardware test loop against Parallels, driven entirely by
# `prlctl` (Parallels Desktop's CLI) - no human watching the VM live, no
# physical keyboard.
#
# What this replaces: every real-hardware round trip documented in
# docs/xhci-keyboard-postmortem.md and docs/boot-bringup-postmortem.md was
# manual - rebuild, re-image, boot the VM, look at the screen, type on a
# real keyboard, report back. `prlctl send-key-event` sends key events to
# a running VM by scancode, and `prlctl capture` grabs a screenshot to a
# file - together, enough to script the same loop. `send-key-event` goes
# through Parallels' own synthetic keyboard device, not the specific
# physical USB keyboard the xHCI postmortem is about, but a live test
# confirmed it lands on the exact same driver code path (xhci::poll_key's
# interrupt endpoint, not a different input mechanism) - see the raw HID
# reports in a captured screenshot if you want to check this again.
#
# Requires the VM already registered in Parallels and its Hard Disk
# device already pointed at this repo's esp.hdd (see the Makefile's
# `parallels-hdd` target's doc comment for how that .hdd gets built).
#
# Usage (normally via `make test-parallels`, see the Makefile target):
#   VM_NAME=Ouroboros CMDS="help;echo hi;uptime" BOOT_WAIT=12 ./scripts/test-parallels.sh
set -eu

VM_NAME="${VM_NAME:-Ouroboros}"
CMDS="${CMDS:-help}"
BOOT_WAIT="${BOOT_WAIT:-12}"
KEY_DELAY="${KEY_DELAY:-0.12}"
CMD_SETTLE="${CMD_SETTLE:-1}"

command -v prlctl >/dev/null 2>&1 || {
	echo "prlctl not found - is Parallels Desktop installed?" >&2
	exit 1
}

prlctl status "$VM_NAME" >/dev/null 2>&1 || {
	echo "no VM named '$VM_NAME' is registered in Parallels (see 'prlctl list -a')" >&2
	exit 1
}

OUT_DIR="parallels-test-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT_DIR"
echo "==> screenshots and log will land in $OUT_DIR/"

# Standard PC AT Set-1 make-code scancodes, decimal (prlctl rejects the
# 0x.. form - confirmed the hard way). Covers what this project's shell
# commands actually need: lowercase letters, digits, space, '/', '.',
# '-', backspace, and enter, plus '>' as a real held-Shift chord (see
# shifted_base/send_char below). Anything outside this set is skipped
# with a warning rather than failing the whole run.
scancode() {
	case "$1" in
	a) echo 30 ;; b) echo 48 ;; c) echo 46 ;; d) echo 32 ;; e) echo 18 ;;
	f) echo 33 ;; g) echo 34 ;; h) echo 35 ;; i) echo 23 ;; j) echo 36 ;;
	k) echo 37 ;; l) echo 38 ;; m) echo 50 ;; n) echo 49 ;; o) echo 24 ;;
	p) echo 25 ;; q) echo 16 ;; r) echo 19 ;; s) echo 31 ;; t) echo 20 ;;
	u) echo 22 ;; v) echo 47 ;; w) echo 17 ;; x) echo 45 ;; y) echo 21 ;;
	z) echo 44 ;;
	0) echo 11 ;; 1) echo 2 ;; 2) echo 3 ;; 3) echo 4 ;; 4) echo 5 ;;
	5) echo 6 ;; 6) echo 7 ;; 7) echo 8 ;; 8) echo 9 ;; 9) echo 10 ;;
	' ') echo 57 ;;
	/) echo 53 ;;
	.) echo 52 ;;
	-) echo 12 ;;
	*) echo "" ;;
	esac
}

# Characters that need a held Shift: maps each to its *base* key's
# scancode; send_char wraps it in explicit Shift press/release events
# (prlctl's --event flag - the default, flagless form sends a full
# press+release for the one key, which can't express a chord).
shifted_base() {
	case "$1" in
	'>') echo 52 ;; # Shift + '.'
	*) echo "" ;;
	esac
}

# Sends one character's press+release, or backspace (BKSP)/enter (ENTER)
# as pseudo-tokens, one scancode per call - prlctl has no "type a string"
# primitive of its own.
send_char() {
	code=$(scancode "$1")
	if [ -n "$code" ]; then
		prlctl send-key-event "$VM_NAME" --scancode "$code" >/dev/null
		sleep "$KEY_DELAY"
		return
	fi
	base=$(shifted_base "$1")
	if [ -z "$base" ]; then
		echo "    (skipping unsupported character: '$1')" >&2
		return
	fi
	prlctl send-key-event "$VM_NAME" --scancode 42 --event press >/dev/null # left Shift down
	prlctl send-key-event "$VM_NAME" --scancode "$base" >/dev/null
	prlctl send-key-event "$VM_NAME" --scancode 42 --event release >/dev/null
	sleep "$KEY_DELAY"
}

send_enter() {
	prlctl send-key-event "$VM_NAME" --scancode 28 >/dev/null
	sleep "$KEY_DELAY"
}

type_line() {
	line="$1"
	i=0
	len=${#line}
	while [ "$i" -lt "$len" ]; do
		ch=$(printf '%s' "$line" | cut -c$((i + 1)))
		send_char "$ch"
		i=$((i + 1))
	done
	send_enter
}

slug() {
	printf '%s' "$1" | tr -c 'a-zA-Z0-9' '_' | cut -c1-40
}

echo "==> rebuilding esp.hdd from the current source"
if prlctl status "$VM_NAME" 2>/dev/null | grep -q running; then
	echo "==> $VM_NAME is currently running - stopping it first (esp.hdd can't be rewritten while it's attached)"
	prlctl stop "$VM_NAME" --kill >/dev/null 2>&1 || true
	sleep 2
fi
make parallels-hdd

echo "==> starting $VM_NAME"
prlctl start "$VM_NAME" >/dev/null

cleanup() {
	echo "==> stopping $VM_NAME"
	prlctl stop "$VM_NAME" --kill >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> waiting ${BOOT_WAIT}s for boot"
sleep "$BOOT_WAIT"

prlctl capture "$VM_NAME" --file "$OUT_DIR/00-boot.png" >/dev/null
echo "==> captured $OUT_DIR/00-boot.png"

step=1
old_ifs=$IFS
IFS=';'
for cmd in $CMDS; do
	IFS=$old_ifs
	cmd=$(printf '%s' "$cmd" | sed -e 's/^ *//' -e 's/ *$//')
	[ -z "$cmd" ] && continue
	echo "==> typing: $cmd"
	if [ "$cmd" = "CTRL-C" ]; then
		# Pseudo-command: a real held-Ctrl+C chord (no Enter), same
		# --event press/release technique as the Shift chord above.
		# LCtrl's Set-1 make code is 29.
		prlctl send-key-event "$VM_NAME" --scancode 29 --event press >/dev/null
		prlctl send-key-event "$VM_NAME" --scancode 46 >/dev/null # 'c'
		prlctl send-key-event "$VM_NAME" --scancode 29 --event release >/dev/null
		sleep "$KEY_DELAY"
	else
		type_line "$cmd"
	fi
	sleep "$CMD_SETTLE"
	file="$OUT_DIR/$(printf '%02d' "$step")-$(slug "$cmd").png"
	prlctl capture "$VM_NAME" --file "$file" >/dev/null
	echo "    captured $file"
	step=$((step + 1))
	IFS=';'
done
IFS=$old_ifs

echo "==> done - inspect the screenshots in $OUT_DIR/"
