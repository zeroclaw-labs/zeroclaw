PORTNAME=	zeroclaw
DISTVERSIONPREFIX=	v
DISTVERSION=	0.8.0-beta-2
CATEGORIES=	misc

MAINTAINER=	jperlow@gmail.com
COMMENT=	Fast, small AI assistant and agent CLI written in Rust
WWW=		https://github.com/zeroclaw-labs/zeroclaw

LICENSE=	APACHE20 MIT
LICENSE_COMB=	dual
LICENSE_FILE_APACHE20=	${WRKSRC}/LICENSE-APACHE
LICENSE_FILE_MIT=	${WRKSRC}/LICENSE-MIT

USES=		cargo
USE_GITHUB=	yes
GH_ACCOUNT=	zeroclaw-labs

# DISTVERSION tracks the upstream release tag. Like the AUR and scoop targets,
# the version is not synced from the repo's bump script — the ports distfile
# scanner (PORTSCOUT) watches upstream tags so the maintainer is notified of a
# new release to bump and re-run `make cargo-crates-merge`.
PORTSCOUT=	limit:^v

# The crate list (CARGO_CRATES) and distinfo are NOT authored here. They are
# generated from the project's Cargo.lock on a FreeBSD host with the framework's
# own targets and are intentionally not committed (see .gitignore). To produce a
# buildable port from this directory:
#
#     make cargo-crates-merge   # reads Cargo.lock -> CARGO_CRATES + distinfo
#     make install clean
#
# On a version bump, re-run `make cargo-crates-merge`; nothing is hand-edited.

# A prebuilt package (`pkg install`) is already compiled; feature selection is a
# source-build choice only. The package is built with the workspace's
# full-channel feature alias so every channel is present; cargo resolves the
# alias members from Cargo.toml. A from-source build can override CARGO_FEATURES.
CARGO_FEATURES?=	channels-full

# zeroclaw installs its own binary plus the acp-bridge bin (default feature).
# zerocode is a separate workspace app (apps/zerocode), built and installed in
# post-build/post-install below. This mirrors install.sh's fixed DEFAULT_APPS
# ("zerocode"); the Tauri app (zeroclaw-desktop) is excluded from this simple
# cargo path because it needs the Tauri toolchain + webview deps.
PLIST_FILES=	bin/zeroclaw \
		bin/zeroclaw-acp-bridge \
		bin/zerocode

# Build and install zerocode (apps/zerocode). cargo.mk's do-build builds the
# root zeroclaw package; this builds zerocode in the same vendored workspace
# with the same CARGO_ENV. cargo.mk sets no cross-compile target dir, so the
# release artifact lands in ${CARGO_TARGET_DIR}/release. zerocode has no cargo
# features of its own, so CARGO_FEATURES is not forwarded to its build.
post-build:
	@${CARGO_CARGO_RUN} build \
		--manifest-path ${CARGO_CARGOTOML} \
		--release \
		-p zerocode

post-install:
	${INSTALL_PROGRAM} ${CARGO_TARGET_DIR}/release/zerocode \
		${STAGEDIR}${PREFIX}/bin/zerocode

.include <bsd.port.mk>
