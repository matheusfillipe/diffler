class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.23.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.1/diffler-v0.23.1-aarch64-apple-darwin.tar.gz"
      sha256 "737fee3f6b4a01b1f97893aa67b1d8287466015d6adc576df5aa55a0b5781d90"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.1/diffler-v0.23.1-x86_64-apple-darwin.tar.gz"
      sha256 "14e9ab4d5c5fed73b7d13266138cb6f34c624f97491d2123e88d8df417ba1d90"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.1/diffler-v0.23.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0111977a37c2168388cdeb7f48a097bf51eb14896b022af8e8e0248b03d48c2c"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.1/diffler-v0.23.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "cd693d5b428e4cc355304018514908dfaffd586ca0b6959adfd087f2236a1d1a"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
