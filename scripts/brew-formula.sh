#!/bin/sh
# Print the Homebrew formula for a release, from that release's signed SHA256SUMS.
#   scripts/brew-formula.sh 0.3.3 > Formula/playbox.rb
# The formula installs the release's own binary (the same archive `install.sh` verifies); `brew audit` and `brew install` both check its sha256.
set -eu
V="${1:?usage: brew-formula.sh VERSION}"
REPO="${PLAYBOX_REPO:-Epistetechnician/playbox}"
SUMS="$(curl -fsSL "https://github.com/$REPO/releases/download/v$V/SHA256SUMS")"
sha() { printf '%s\n' "$SUMS" | awk -v f="playbox-$1.tar.gz" '$2==f{print $1}'; }
for t in darwin-arm64 darwin-x86_64 linux-arm64 linux-x86_64; do [ -n "$(sha $t)" ] || { echo "no checksum for $t in v$V" >&2; exit 1; }; done
url() { echo "https://github.com/$REPO/releases/download/v$V/playbox-$1.tar.gz"; }
cat <<EOF
class Playbox < Formula
  desc "Private rooms for agents from different owners, with signed, verifiable history"
  homepage "https://github.com/$REPO"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "$(url darwin-arm64)"
      sha256 "$(sha darwin-arm64)"
    end
    on_intel do
      url "$(url darwin-x86_64)"
      sha256 "$(sha darwin-x86_64)"
    end
  end

  on_linux do
    on_arm do
      url "$(url linux-arm64)"
      sha256 "$(sha linux-arm64)"
    end
    on_intel do
      url "$(url linux-x86_64)"
      sha256 "$(sha linux-x86_64)"
    end
  end

  def install
    bin.install "playbox"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/playbox --version")
  end
end
EOF
