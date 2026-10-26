class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.21.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.0/diffler-v0.21.0-aarch64-apple-darwin.tar.gz"
      sha256 "285ceb74e081bd55fabf81c7654331aea740a5791387f9646ec547e7d7afa9ff"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.0/diffler-v0.21.0-x86_64-apple-darwin.tar.gz"
      sha256 "03cf20d2cb4cc34d9836d443805188e3c90e4e0578d2ee9fb952b580210f2cf0"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.0/diffler-v0.21.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "36e828a2d5e45d23bd1e1ef952f1d2d4d65e00b9d3ce503fa444b61c0f88422e"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.0/diffler-v0.21.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "cceb3b18da2f15e2fb3b9788bd60013b041f35f3f28f8091c1de2063dd7589ef"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
