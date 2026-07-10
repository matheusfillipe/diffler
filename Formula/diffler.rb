class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.4.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.1/diffler-v0.4.1-aarch64-apple-darwin.tar.gz"
      sha256 "2b62b3633a6cbbd525e982e8b60bdbdd2dfb03cc78c31ac29dde42809f1569c4"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.1/diffler-v0.4.1-x86_64-apple-darwin.tar.gz"
      sha256 "fbd877dbd9468f018a0817fa6290bdc5337b9e68e6ce6d221ea075997b3fe40c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.1/diffler-v0.4.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "de84fb56827d2834891bf1e8c336f51e5422cd9c832435694ce6e5afc70bed2d"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.1/diffler-v0.4.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "a5a06d1f3a7f7e68f4ef88b6c11b26acc9666647b218298a156764f7af7a8177"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
