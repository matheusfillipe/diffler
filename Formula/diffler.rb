class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.9.2"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.2/diffler-v0.9.2-aarch64-apple-darwin.tar.gz"
      sha256 "540f69a4af9ef66e4ddb2560b20918c0762a57389f0be5772bb0506532b0ef41"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.2/diffler-v0.9.2-x86_64-apple-darwin.tar.gz"
      sha256 "71c13a6e78527dfe5241f66812903e1dc422c64b869095f61553627d72fcf0f3"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.2/diffler-v0.9.2-aarch64-unknown-linux-musl.tar.gz"
      sha256 "b7f4adb18f6d2ec7f0cc7895488415968dd5f548751598fef7b4cb934faee3ec"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.2/diffler-v0.9.2-x86_64-unknown-linux-musl.tar.gz"
      sha256 "89df187f3df153bdbcf50ca96f31ac8f37261cdf5e70fe652b3ece56b4353a55"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
