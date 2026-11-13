class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.23.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.0/diffler-v0.23.0-aarch64-apple-darwin.tar.gz"
      sha256 "5b38f5ef6db82730a26e33f73fea58af621a9466c6168c464b4af4032119008f"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.0/diffler-v0.23.0-x86_64-apple-darwin.tar.gz"
      sha256 "87bc27a3d5386e6add7fc7263246c41c60753eccdb53a4b8edfa5686e010bc96"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.0/diffler-v0.23.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "103528e72a2036be6f7b29bc454bee8cac9bb5d50b7f39bf2c0890caf7d5a634"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.0/diffler-v0.23.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "67ad517e83cfa7c4a4f352f2dccedd115fb6ea2577910c6dd231519942a04874"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
