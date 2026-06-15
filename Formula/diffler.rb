class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.6"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.6/diffler-v0.1.6-aarch64-apple-darwin.tar.gz"
      sha256 "cfe0ddffd9824faf91cb145400baba7964f5eff2881ae6011d24aff5b5010bcb"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.6/diffler-v0.1.6-x86_64-apple-darwin.tar.gz"
      sha256 "69bdc249563cd120cb41f2fc8a0c860f94d386238d0bf09e60abe899b14221b6"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.6/diffler-v0.1.6-aarch64-unknown-linux-musl.tar.gz"
      sha256 "879ee0a8f1578158bef8ead16e90b1a5096b426c5a617b2395c684436dadf32c"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.6/diffler-v0.1.6-x86_64-unknown-linux-musl.tar.gz"
      sha256 "c881739b75f69c2b79109e30bfb908eb01a2424098998f68084f15f463087659"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
