class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.13.2"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.2/diffler-v0.13.2-aarch64-apple-darwin.tar.gz"
      sha256 "5e56df588e1573fb591e98ca3c69b16d62b8df8cccd039d26588e30ecaaee394"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.2/diffler-v0.13.2-x86_64-apple-darwin.tar.gz"
      sha256 "9f24de08275af2dcfd0d05d174b97568b95b19b22bc868c528f76edd5bfcdcb6"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.2/diffler-v0.13.2-aarch64-unknown-linux-musl.tar.gz"
      sha256 "8f4566240ac2d3b69c98b256bfbc6790c39b93245973fbbf6a0ea14aed602206"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.13.2/diffler-v0.13.2-x86_64-unknown-linux-musl.tar.gz"
      sha256 "062caff4229e5f17efe3d4c6c33c80fd810ccfa95e246134f7fd454d810c9943"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
