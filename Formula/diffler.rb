class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.4"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.4/diffler-v0.6.4-aarch64-apple-darwin.tar.gz"
      sha256 "15789c30493fee7ec4bd4e65a17fc2e5284d64a4c1362e1b5bae672661aaf4b7"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.4/diffler-v0.6.4-x86_64-apple-darwin.tar.gz"
      sha256 "ec4f5a65fc920b57d7c4cea39dbe3aba73ed2457e7394ef1079150f5bd9093e8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.4/diffler-v0.6.4-aarch64-unknown-linux-musl.tar.gz"
      sha256 "eeba672fc86905b82a0bd8df1f7e56903fdbfb9544d5d90650f692b1c55e2129"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.4/diffler-v0.6.4-x86_64-unknown-linux-musl.tar.gz"
      sha256 "4be5ab92b4b831693ea67a5fc4a11d03a31fdd7e222dd1d3813d05bbef1bca51"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
