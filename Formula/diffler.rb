class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.19.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.0/diffler-v0.19.0-aarch64-apple-darwin.tar.gz"
      sha256 "49a36078270c085ffd64b93cf4d71568d3b5a80f62ff68f3e3c4b202e93bc23a"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.0/diffler-v0.19.0-x86_64-apple-darwin.tar.gz"
      sha256 "bc573d2b9ef419a7a19c2cef20eeeb5a4a0cb3effcfa43ce9f450639ac3913d3"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.0/diffler-v0.19.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "c02e79bdd166b0de9ba25c21e6a2fed227f2acad353e6a6e00acfd2f0d58f97c"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.0/diffler-v0.19.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "7dbdca8f05cda2d037541aa638b6027e011553ecf61c679cc8412b3fe818f733"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
