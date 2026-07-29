class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.6"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.6/diffler-v0.6.6-aarch64-apple-darwin.tar.gz"
      sha256 "584e7b98e7249da2ba89dc7647892ec0d815613420b985fe41e91c629c4a223c"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.6/diffler-v0.6.6-x86_64-apple-darwin.tar.gz"
      sha256 "eff84e468b80eaded97a4c88f7b56e0217b63e6b474b87bd56dff7e710b5efc4"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.6/diffler-v0.6.6-aarch64-unknown-linux-musl.tar.gz"
      sha256 "4f2de2e6b283cfe0e74771f16989fb3b155849499fa22ac8f24e62aac013e0e4"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.6/diffler-v0.6.6-x86_64-unknown-linux-musl.tar.gz"
      sha256 "c8aa8f7ac29c2e5583d252555a97c808defb22674e7df9ede21f7ff9435f6e8f"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
