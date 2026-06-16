class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.13"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.13/diffler-v0.1.13-aarch64-apple-darwin.tar.gz"
      sha256 "84fb533fab5011d85895f0a4af0de03aaffe7a08ad0af4d4c7ea7d9d9f4e8360"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.13/diffler-v0.1.13-x86_64-apple-darwin.tar.gz"
      sha256 "9153927aeeecea00b85f5aa22ab3823d6ff01eb3d51f48eb6fd7bf5ad5a888cb"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.13/diffler-v0.1.13-aarch64-unknown-linux-musl.tar.gz"
      sha256 "8da02c6aee4e68da83f95dced991e2dfabcf4784c576f3105872c4e3150140bf"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.13/diffler-v0.1.13-x86_64-unknown-linux-musl.tar.gz"
      sha256 "3f0de8b9c44555979993b1a9ab1d4037970001d6f91fb2d6d3ff076f66f6de06"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
