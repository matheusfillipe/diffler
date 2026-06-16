class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.11"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.11/diffler-v0.1.11-aarch64-apple-darwin.tar.gz"
      sha256 "6633c3b05f224be184694b3ee4290ae835efb26a0f15771c444152ef46271ccf"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.11/diffler-v0.1.11-x86_64-apple-darwin.tar.gz"
      sha256 "abd8b663177bd030f69e4b2451f9c8f48962c37b56b06d327608731c9c01db1a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.11/diffler-v0.1.11-aarch64-unknown-linux-musl.tar.gz"
      sha256 "a9a8808aff1c48421443578c9df1f2375ae7bded385852b59ab845c3a8d2b39b"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.11/diffler-v0.1.11-x86_64-unknown-linux-musl.tar.gz"
      sha256 "b7695b6eeb5326c591d330a5fcc9f1da4d90957953d994d5019c480701be1ee6"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
