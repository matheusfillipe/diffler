class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.22.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.22.0/diffler-v0.22.0-aarch64-apple-darwin.tar.gz"
      sha256 "5c8cc037653fc1d9e0c9d2958a4b92438aafc5fdaea98c64394d9264a0e40322"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.22.0/diffler-v0.22.0-x86_64-apple-darwin.tar.gz"
      sha256 "f4004b57ac7e97a560613cbaa8808daee2f4964d6be1034826d15a93e2e5660f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.22.0/diffler-v0.22.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "ec4502ae76b5a8bc28a2d800f43bc3ab5ea58b6d18c30f46367b135ecedc7e18"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.22.0/diffler-v0.22.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "5b3f79d04e73af01d4659efb7411dee494fcd9a40280aa387b3c22e91a21cc34"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
