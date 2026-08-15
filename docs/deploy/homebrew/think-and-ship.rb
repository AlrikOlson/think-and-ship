# Homebrew formula TEMPLATE for think-and-ship.
#
# This file is a starting point — it does NOT live in the tap. To publish:
#   1. Create a tap repo: `AlrikOlson/homebrew-tap`.
#   2. Copy this file to `Formula/think-and-ship.rb` in that repo.
#   3. Fill in VERSION and the four sha256 values for the release tarballs
#      (see docs/RELEASING.md → "Homebrew tap"). The sha256 of each tarball is
#      printed by the release.yml run, or computed locally with:
#        shasum -a 256 think-and-ship-vVERSION-<target>.tar.gz
#
# Once published, users install with:
#   brew install alrikolson/tap/think-and-ship
#
# The release.yml binaries are the source: it produces
#   think-and-ship-vVERSION-<target>.tar.gz
# for each target below, attached to the GitHub release for the tag.

class ThinkAndShip < Formula
  desc "One MCP server, two halves: structured reasoning (think) + execution tracking (ship)"
  homepage "https://github.com/AlrikOlson/think-and-ship"
  version "VERSION" # e.g. "0.3.0" — no leading "v"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/AlrikOlson/think-and-ship/releases/download/v#{version}/think-and-ship-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_SHA256_aarch64-apple-darwin"
    end
    on_intel do
      url "https://github.com/AlrikOlson/think-and-ship/releases/download/v#{version}/think-and-ship-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_SHA256_x86_64-apple-darwin"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/AlrikOlson/think-and-ship/releases/download/v#{version}/think-and-ship-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_SHA256_aarch64-unknown-linux-gnu"
    end
    on_intel do
      url "https://github.com/AlrikOlson/think-and-ship/releases/download/v#{version}/think-and-ship-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_SHA256_x86_64-unknown-linux-gnu"
    end
  end

  def install
    bin.install "think-and-ship"
  end

  test do
    assert_match "think-and-ship", shell_output("#{bin}/think-and-ship --version")
  end
end
