class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.13.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.1/diffler-v0.13.1-aarch64-apple-darwin.tar.gz"
      sha256 "932cb6717fba859ad241759d0b734e1eba11e5f4351ff1d62784df932b1cef92"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.1/diffler-v0.13.1-x86_64-apple-darwin.tar.gz"
      sha256 "cad0ab0a86a85d159f5d955cd1332ac578096ee470ca05b820502e6f3239ee2b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.1/diffler-v0.13.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "7fc35bfc70770ee04a52addc7183ce8f9f7f604d3185c1efacd4585aa9987b29"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.1/diffler-v0.13.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "fb01a92c0bb6a5967342c44c64a4297de4b1a684a62a4404cf77cd1094a35008"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
