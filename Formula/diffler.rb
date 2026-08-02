class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.9"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.9/diffler-v0.6.9-aarch64-apple-darwin.tar.gz"
      sha256 "bbf3546e132b2c9faa3f5be111940799e054ca3cd8e5e739cd2cd30a86ef6878"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.9/diffler-v0.6.9-x86_64-apple-darwin.tar.gz"
      sha256 "5019e0d897f0651ad68b39220cae0de2ce2e07ccff178ac0fd65f5c53a4ccc74"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.9/diffler-v0.6.9-aarch64-unknown-linux-musl.tar.gz"
      sha256 "65cd19ee6416be5c16ec71c4c1c6edd5129686e4dbf9a0180092d7d3e69e2170"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.9/diffler-v0.6.9-x86_64-unknown-linux-musl.tar.gz"
      sha256 "b50ea0058e8ccbfd7a8e2da43379ca82c9504196b5c2b51455ef98cca3ae6538"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
