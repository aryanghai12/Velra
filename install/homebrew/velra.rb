# Homebrew formula for Velra.
#
# Template for the aryanghai12/homebrew-tap formula. The version and sha256
# values below are placeholders, filled in from the release checksums when the
# tap is updated; publishing the tap is not yet automated, so this file is not
# an installable formula as it stands.
#
#   brew install aryanghai12/tap/velra
class Velra < Formula
  desc "Local-first session continuity for Claude Code"
  homepage "https://github.com/aryanghai12/Velra"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/aryanghai12/velra/releases/download/v#{version}/velra-aarch64-apple-darwin.tar.gz"
      sha256 "{{SHA256_DARWIN_ARM64}}"
    end
    on_intel do
      url "https://github.com/aryanghai12/velra/releases/download/v#{version}/velra-x86_64-apple-darwin.tar.gz"
      sha256 "{{SHA256_DARWIN_X64}}"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/aryanghai12/velra/releases/download/v#{version}/velra-aarch64-unknown-linux-musl.tar.gz"
      sha256 "{{SHA256_LINUX_ARM64}}"
    end
    on_intel do
      url "https://github.com/aryanghai12/velra/releases/download/v#{version}/velra-x86_64-unknown-linux-musl.tar.gz"
      sha256 "{{SHA256_LINUX_X64}}"
    end
  end

  def install
    bin.install "velra"
  end

  def caveats
    <<~EOS
      Register Velra's hooks with Claude Code:
        velra enable

      Velra edits only your user-level Claude Code settings, keeps a backup in
      ~/.velra/backups, and `velra disable` restores it byte for byte.
    EOS
  end

  test do
    assert_match "velra", shell_output("#{bin}/velra --version")
    assert_match "Enabled", shell_output("#{bin}/velra status", 1)
  end
end
