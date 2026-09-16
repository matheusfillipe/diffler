class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.15.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.15.0/diffler-v0.15.0-aarch64-apple-darwin.tar.gz"
      sha256 "560a0fcec8822098a978499cce0eb70a4f94e7ecaf17195b0dd2bb466daa2ad4"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.15.0/diffler-v0.15.0-x86_64-apple-darwin.tar.gz"
      sha256 "44047bc75971fe9278cb87bafdec6b539d1db7a15ab987d161634a0366a84c43"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.15.0/diffler-v0.15.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0a4d5e1eb48d6f9c264c13e90a3c51e1d5f9cd0a8897e59ea3e0542c5c781222"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.15.0/diffler-v0.15.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "2469199688351c2a801536838ed37f0f533bcd2299982724e2c16c1f17526d45"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
