class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.11.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.1/diffler-v0.11.1-aarch64-apple-darwin.tar.gz"
      sha256 "4c1878ca04b3da5b18d28add69bf6f1e61d4ad2f77ef570d744a069e9023f377"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.1/diffler-v0.11.1-x86_64-apple-darwin.tar.gz"
      sha256 "57e75c106da88f3839d6114ad325cb1f79dae524e7a01dbf15c07a989abb21d7"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.1/diffler-v0.11.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "13bc41c9976ea1f129ad156b9f7e9c1f9ffd7010c05e8c28b62cc7d51601e618"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.1/diffler-v0.11.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "b7abb61a1385245249272c143d103266dc4c1de7989987b05af9983a4dbb45f7"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
