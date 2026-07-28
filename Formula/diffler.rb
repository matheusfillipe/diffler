class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.5"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.5/diffler-v0.6.5-aarch64-apple-darwin.tar.gz"
      sha256 "cfd9307d907e711e0827428ae79d934e0cccd0ab66671ab303208477c0ad49da"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.5/diffler-v0.6.5-x86_64-apple-darwin.tar.gz"
      sha256 "a7c3fd5e7e2d52dd5713b299f97dc8e7164778fb45ec0267a1b7f70c02a1ea63"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.5/diffler-v0.6.5-aarch64-unknown-linux-musl.tar.gz"
      sha256 "853821f71eb4171a7bce31d497287b573328d18ed8fca82ff57053abf14ce9d9"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.5/diffler-v0.6.5-x86_64-unknown-linux-musl.tar.gz"
      sha256 "b3a07f683f3bf350803e3b1778b8d4da29114f67bf7d091badae539f2bf80fc9"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
