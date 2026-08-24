#!/usr/bin/env bash
#
# release.sh - cut an Ouroboros release.
#
# Two phases, deliberately split so the outward-facing half is never a
# side effect of the local half:
#
#   scripts/release.sh build     - build the release-profile disk images
#                                  and package the downloadable artifacts
#                                  (esp.img.zip + esp.hdd.zip + SHA256SUMS).
#                                  Purely local; safe to run any time.
#
#   scripts/release.sh publish   - create the annotated git tag, push it,
#                                  and create the GitHub Release with the
#                                  packaged assets + notes. OUTWARD-FACING:
#                                  run only when you actually mean to ship.
#
# The version comes from the top-level VERSION file (single source of
# truth). See docs/RELEASING.md for the whole flow and the version scheme.
#
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"

VERSION="$(tr -d '[:space:]' < VERSION)"
TAG="v${VERSION}"
REL_DIR="build/release/${TAG}"
IMG_ZIP="${REL_DIR}/ouroboros-${VERSION}-esp.img.zip"
# The portable Parallels form is the self-contained UDZO .dmg, NOT the
# .hdd bundle: `prl_disk_tool create --dmg` writes a DiskDescriptor.xml
# that references the .dmg by ABSOLUTE PATH (and leaves the .hdd data
# file empty), so a zipped .hdd is useless off this machine. Ship the
# .dmg; the notes carry the one-line recipe to wrap it into a bootable
# .hdd locally.
DMG_OUT="${REL_DIR}/ouroboros-${VERSION}-esp.dmg"
NOTES="docs/release-notes/${TAG}.md"

log() { printf '\033[1;36m[release]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[release] error:\033[0m %s\n' "$*" >&2; exit 1; }

cmd_build() {
	log "building Ouroboros ${TAG} (release profile)"

	# Real shipped bits are the release profile, not debug.
	log "make image PROFILE=release"
	make image PROFILE=release

	[ -f build/esp.img ] || die "build/esp.img missing after 'make image'"

	# The self-contained UDZO .dmg is the portable Parallels form (see the
	# DMG_OUT comment above): it embeds the disk data, unlike the .hdd
	# bundle which only references it by absolute path.
	log "hdiutil convert -> build/esp.dmg (self-contained Parallels image)"
	rm -f build/esp.dmg
	hdiutil convert build/esp.img -format UDZO -o build/esp.dmg
	[ -f build/esp.dmg ] || die "build/esp.dmg missing"

	rm -rf "${REL_DIR}"
	mkdir -p "${REL_DIR}"

	log "packaging ${IMG_ZIP}"
	# -j: flatten; the .img is a single raw file.
	(cd build && zip -q -j "${ROOT}/${IMG_ZIP}" esp.img)

	log "packaging ${DMG_OUT} (self-contained; already compressed)"
	cp build/esp.dmg "${DMG_OUT}"

	log "SHA256SUMS"
	(cd "${REL_DIR}" && shasum -a 256 ./*.zip ./*.dmg > SHA256SUMS && cat SHA256SUMS)

	log "artifacts ready in ${REL_DIR}"
	ls -lh "${REL_DIR}"
}

cmd_publish() {
	command -v gh >/dev/null || die "gh (GitHub CLI) not found"
	[ -f "${IMG_ZIP}" ] && [ -f "${DMG_OUT}" ] || \
		die "artifacts missing - run 'scripts/release.sh build' first"
	[ -f "${NOTES}" ] || die "release notes missing: ${NOTES}"

	# Refuse to ship a dirty tree or a tag that already exists.
	[ -z "$(git status --porcelain)" ] || die "working tree is dirty - commit first"
	if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
		die "tag ${TAG} already exists"
	fi

	local branch; branch="$(git rev-parse --abbrev-ref HEAD)"
	[ "${branch}" = "main" ] || log "WARNING: publishing from '${branch}', not main"

	log "annotated tag ${TAG}"
	git tag -a "${TAG}" -F "${NOTES}"

	log "push tag ${TAG}"
	git push origin "${TAG}"

	log "gh release create ${TAG}"
	gh release create "${TAG}" \
		--title "Ouroboros ${TAG}" \
		--notes-file "${NOTES}" \
		"${IMG_ZIP}" "${DMG_OUT}" "${REL_DIR}/SHA256SUMS"

	log "published: $(gh release view "${TAG}" --json url -q .url)"
}

case "${1:-}" in
	build)   cmd_build ;;
	publish) cmd_publish ;;
	*) die "usage: $0 {build|publish}   (version ${VERSION} from ./VERSION)" ;;
esac
