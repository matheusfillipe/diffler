class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.1/diffler-v0.6.1-aarch64-apple-darwin.tar.gz"
      sha256 "6cd546762f9f09bc4626806c5db6bb9607d56dae165fb5c491c9e3f44930c5a1"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.1/diffler-v0.6.1-x86_64-apple-darwin.tar.gz"
      sha256 "87ba934ff2f1e6b892683191e3f8dc8d0c2fdd525431bdf14e14843153befde2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.1/diffler-v0.6.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "4d7095fbed773b6621a33370f47df8f306d244732fd1eae7bc01746b157f1f5a"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.1/diffler-v0.6.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d0db644e15bd5b1b68efb2904adc4054a0795c94a471ce903ee5382df5371aa9"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
