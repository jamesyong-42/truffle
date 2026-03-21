# Homebrew formula for truffle
# Install: brew install jamesyong-42/truffle/truffle
#
# This formula downloads pre-built binaries from GitHub releases.
# To use this formula via a tap:
#   brew tap jamesyong-42/truffle
#   brew install truffle

class Truffle < Formula
  desc "Mesh networking for your devices, built on Tailscale"
  homepage "https://github.com/jamesyong-42/truffle"
  version "0.1.1"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/jamesyong-42/truffle/releases/download/v#{version}/truffle-darwin-arm64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_DARWIN_ARM64"
    elsif Hardware::CPU.intel?
      url "https://github.com/jamesyong-42/truffle/releases/download/v#{version}/truffle-darwin-x64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_DARWIN_X64"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/jamesyong-42/truffle/releases/download/v#{version}/truffle-linux-arm64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_LINUX_ARM64"
    elsif Hardware::CPU.intel?
      url "https://github.com/jamesyong-42/truffle/releases/download/v#{version}/truffle-linux-x64.tar.gz"
      sha256 "PLACEHOLDER_SHA256_LINUX_X64"
    end
  end

  depends_on "tailscale" => :recommended

  def install
    bin.install "truffle"
    bin.install "sidecar-slim"
  end

  def caveats
    <<~EOS
      truffle requires Tailscale to be installed and running.
      Install Tailscale from https://tailscale.com/download if you haven't already.

      Quick start:
        tailscale up          # connect to your tailnet
        truffle up             # start your mesh node
        truffle ls             # see other nodes
        truffle doctor         # diagnose issues
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/truffle --version")
  end
end
