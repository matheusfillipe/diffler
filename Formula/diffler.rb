class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.5"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.5/diffler-v0.1.5-aarch64-apple-darwin.tar.gz"
      sha256 "58edccf51cb0d46ea7d7ae5fa8622871fe3f3a45ee12371ad858737d88a80c84"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.5/diffler-v0.1.5-x86_64-apple-darwin.tar.gz"
      sha256 "569475e6483bcec052ba5aaa28c3e6a968b158a47cacc4af27b735c49203358e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.5/diffler-v0.1.5-aarch64-unknown-linux-musl.tar.gz"
      sha256 "8da24b4f74ce5aec129c52627e15a5950aeacbd55e9a97610dde515edc69cc1f"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.5/diffler-v0.1.5-x86_64-unknown-linux-musl.tar.gz"
      sha256 "eba2c4bfff3a7b7a7601b11e903231be4e410861046b05ff3bc718b42b811dec"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
