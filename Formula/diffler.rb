class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.17.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.17.0/diffler-v0.17.0-aarch64-apple-darwin.tar.gz"
      sha256 "3380943ec826092beeeeeb04615641f329c24f1fee9c97b3d0d11f70c360bafc"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.17.0/diffler-v0.17.0-x86_64-apple-darwin.tar.gz"
      sha256 "075d6a19ba142fda1aff441f01b4717b8450093cd251fea2ac61cd989308b05e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.17.0/diffler-v0.17.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "f90d886cbcb342333b3cf871b13c1ef149e0d759b562a0dcdc55a4e956614d89"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.17.0/diffler-v0.17.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "de4a36e7bc2208a8cd71ff97ef6dfd25e01ff6dac5dbe4c8b49102951bc6c87f"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
