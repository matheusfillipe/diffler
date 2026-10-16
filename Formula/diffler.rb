class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.20.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.20.0/diffler-v0.20.0-aarch64-apple-darwin.tar.gz"
      sha256 "3fb048d60f7fba054fff29a15653f0867b1b0cc53fdc8a7bb525c358402b328b"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.20.0/diffler-v0.20.0-x86_64-apple-darwin.tar.gz"
      sha256 "138afa16653ffbb20a8a7a767d1969ec5de01f1b6d613d2c84b391f58964e7a5"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.20.0/diffler-v0.20.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "b79a2a95ddadadeadf4b11c06867a53dbab0cbe05f407fda8bd4be407d6d2136"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.20.0/diffler-v0.20.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "4610b774cee413e956123a567d633c6611a8d423383da2004a34ffc492f92502"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
