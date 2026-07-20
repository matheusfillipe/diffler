class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.3"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.3/diffler-v0.6.3-aarch64-apple-darwin.tar.gz"
      sha256 "6aa7a4a1c0ed6d86263caa513d1b2048c290503502cece30fc3709351c435e16"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.3/diffler-v0.6.3-x86_64-apple-darwin.tar.gz"
      sha256 "67fa57e7d2d92e39106159136954fdbc1c0c295ab84ca5ab60e2d998161bbb99"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.3/diffler-v0.6.3-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0acaed51fc07be937a371f963f31f58bc6009ec4aa29f7a26edfb2cfe6a153bd"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.3/diffler-v0.6.3-x86_64-unknown-linux-musl.tar.gz"
      sha256 "bc96989e5235ca4ce7150c3ede5f81bd12d815da2c8184c8138dd882c88707cb"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
