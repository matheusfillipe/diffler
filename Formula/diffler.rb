class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.9"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.9/diffler-v0.1.9-aarch64-apple-darwin.tar.gz"
      sha256 "0e27cb9e62d76ec1d08cbe2d68c266745456483ae5c1b93faab2331c7f676931"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.9/diffler-v0.1.9-x86_64-apple-darwin.tar.gz"
      sha256 "6f8d054e09a999415731c2bf4a816a1968c4887404578de08f0c1cc68bc2abdb"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.9/diffler-v0.1.9-aarch64-unknown-linux-musl.tar.gz"
      sha256 "4279985b983ff73e775e9b23f5c8b38063caa6f919fb1f55ed9cdb3c3ad1674a"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.9/diffler-v0.1.9-x86_64-unknown-linux-musl.tar.gz"
      sha256 "3f9a2bf7080cfd1c588680a3710b59bc25b3010df18638a0159e6b8df75bb3b7"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
